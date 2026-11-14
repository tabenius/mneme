"""Transport recovery tests; all data is synthetic and all waits are bounded."""
import json
import os
from pathlib import Path
import selectors
import sqlite3
import subprocess
import tempfile
import time
import unittest


class McpRecovery(unittest.TestCase):
    def test_killed_collector_releases_lock_and_restart_handles_sigterm(self):
        binary = str(Path(os.environ.get("MNEME_TEST_BINARY", "target/debug/mneme")).resolve())
        with tempfile.TemporaryDirectory() as directory:
            db = Path(directory) / "index.sqlite"
            credential = Path(directory) / "credential.json"
            credential.write_text('{"password":"synthetic"}')
            command = [binary, "--db", str(db), "collect", "--follow",
                       "--url", "http://127.0.0.1:0", "--password-file", str(credential)]

            def wait_started(process):
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    self.assertIsNone(process.poll(), "collector exited while upstream was unavailable")
                    if db.exists():
                        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
                            try:
                                row = connection.execute("SELECT connection FROM sources").fetchone()
                                if row and row[0] == "degraded":
                                    return
                            except sqlite3.OperationalError:
                                pass  # Atomic schema creation may still be underway.
                    time.sleep(0.05)
                self.fail("collector did not initialize")

            for crash in (True, False):
                process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    wait_started(process)
                    # Startup health survives SIGKILL, so allow the restarted
                    # process time to acquire its lock and enter verification.
                    time.sleep(0.2)
                    self.assertIsNone(process.poll())
                    if crash:
                        process.kill()
                        self.assertLess(process.wait(timeout=5), 0)
                    else:
                        process.terminate()
                        self.assertEqual(process.wait(timeout=5), 0)
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
            with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
                self.assertEqual(connection.execute("PRAGMA quick_check").fetchone()[0], "ok")

    def test_missing_corrupt_then_recreated_index_on_same_connection(self):
        binary = str(Path(os.environ.get("MNEME_TEST_BINARY", "target/debug/mneme")).resolve())
        with tempfile.TemporaryDirectory() as directory:
            db = Path(directory) / "index.sqlite"
            process = subprocess.Popen(
                [binary, "--db", str(db), "mcp"],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)
            buffered = b""

            def request(method, params, identity):
                nonlocal buffered
                message = {"jsonrpc": "2.0", "method": method, "params": params}
                if identity is not None:
                    message["id"] = identity
                process.stdin.write(json.dumps(message).encode() + b"\n")
                process.stdin.flush()
                if identity is None:
                    return
                deadline = time.monotonic() + 10
                while True:
                    while b"\n" in buffered:
                        line, buffered = buffered.split(b"\n", 1)
                        response = json.loads(line)
                        if response.get("id") == identity:
                            self.assertNotIn("error", response)
                            return response["result"]
                    self.assertTrue(selector.select(max(0, deadline - time.monotonic())), "MCP timeout")
                    chunk = os.read(process.stdout.fileno(), 65536)
                    self.assertTrue(chunk, "MCP closed stdout")
                    buffered += chunk

            def status(identity):
                return request("tools/call", {"name": "history_index_status", "arguments": {}}, identity)

            try:
                request("initialize", {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "recovery-test", "version": "1"},
                }, 1)
                request("notifications/initialized", {}, None)
                self.assertEqual(len(request("tools/list", {}, 2)["tools"]), 10)
                missing = status(3)
                self.assertFalse(missing["isError"])
                self.assertEqual(missing["structuredContent"]["state"], "missing")
                self.assertFalse(missing["structuredContent"]["ok"])
                self.assertIn(
                    "mneme-collector", missing["structuredContent"]["remedy"]
                )
                # Data tools still fail, with a stable machine-readable code.
                data = request(
                    "tools/call",
                    {"name": "history_list_sessions", "arguments": {}},
                    30,
                )
                self.assertTrue(data["isError"])
                self.assertEqual(data["structuredContent"]["code"], "index_missing")
                self.assertFalse(db.exists(), "MCP must not create an empty index")
                db.write_bytes(b"synthetic corruption")
                corrupt = status(4)
                self.assertFalse(corrupt["isError"])
                self.assertEqual(
                    corrupt["structuredContent"]["state"], "unreadable"
                )
                self.assertTrue(corrupt["structuredContent"]["index"]["exists"])
                db.unlink()
                credential = Path(directory) / "credential.json"
                credential.write_text('{"password":"synthetic"}')
                # Port 0 fails promptly, after collector schema initialization.
                result = subprocess.run([
                    binary, "--db", str(db), "collect", "--url", "http://127.0.0.1:0",
                    "--password-file", str(credential),
                ], capture_output=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(status(5).get("isError", False))
                self.assertIsNone(process.poll())
                process.stdin.close()
                self.assertEqual(process.wait(timeout=5), 0)
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
                selector.close()
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()
