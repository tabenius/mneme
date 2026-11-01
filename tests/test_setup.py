"""Configuration tests use temporary homes and never touch a running OpenCode."""
import importlib.util
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("setup_local", ROOT / "scripts/setup-local.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class SetupTests(unittest.TestCase):
    def test_stage_preserves_config_and_disables_mcp(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            config = home / ".config/opencode/opencode.json"
            config.parent.mkdir(parents=True)
            original = {"$schema": "https://opencode.ai/config.json", "model": "test/model", "mcp": {"other": {"enabled": False}}}
            config.write_text(json.dumps(original))
            result = setup.configure(home, home / ".local/bin/mneme", home / "data/index.sqlite", "http://127.0.0.1:4096", "synthetic-only", False)
            value = json.loads(config.read_text())
            self.assertFalse(value["mcp"]["mneme"]["enabled"])
            self.assertEqual(value["model"], "test/model")
            self.assertEqual(value["mcp"]["other"], {"enabled": False})
            self.assertFalse(result["mcpEnabled"])
            self.assertEqual((home / ".config/mneme/collector.env").stat().st_mode & 0o777, 0o600)
            self.assertEqual((home / ".config/mneme/opencode-service.json").stat().st_mode & 0o777, 0o600)
            self.assertEqual(len(list(config.parent.glob("*.mneme-backup-*"))), 1)
            self.assertFalse((home / "data/index.sqlite").exists())

    def test_enable_compatible_server_and_preserve_new_config_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            config = home / ".config/opencode/opencode.json"
            config.parent.mkdir(parents=True)
            config.write_text(json.dumps({"mcp": {"servers": {"other": {"disabled": True}}}}))
            setup.configure(home, home / ".local/bin/mneme", home / "data/index.sqlite", "http://127.0.0.1:4096", "synthetic-only", True)
            value = json.loads(config.read_text())
            self.assertFalse(value["mcp"]["servers"]["mneme"]["disabled"])
            self.assertEqual(value["mcp"]["servers"]["other"], {"disabled": True})

    def test_no_shadowing_jsonc_and_no_replacing_other_mneme_entry(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            config = home / ".config/opencode/opencode.jsonc"
            config.parent.mkdir(parents=True)
            config.write_text("// owner configuration\n{}")
            with self.assertRaisesRegex(ValueError, "jsonc"):
                setup.configure(home, home / "bin/mneme", home / "data/index.sqlite", "http://127.0.0.1:4096", "synthetic", True)
            self.assertFalse(config.with_suffix(".json").exists())
            config.unlink()
            config = config.with_suffix(".json")
            config.write_text(json.dumps({"mcp": {"mneme": {"command": ["other-mneme"]}}}))
            with self.assertRaisesRegex(ValueError, "different Mneme"):
                setup.configure(home, home / "bin/mneme", home / "data/index.sqlite", "http://127.0.0.1:4096", "synthetic", True)

    def test_incompatible_server_never_starts_services_even_when_requested(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            binary = home / "mneme"
            binary.touch()
            password = home / "credential.json"
            password.write_text('{"password":"synthetic-only"}')
            args = ["setup-local.py", "--url", "http://127.0.0.1:4096", "--password-file", str(password),
                    "--binary", str(binary), "--stage", "--start-collector", "--opencode-unit", "opencode-test.service"]
            output = io.StringIO()
            with mock.patch("sys.argv", args), mock.patch.object(setup.Path, "home", return_value=home), \
                 mock.patch.object(setup.preflight, "inspect", return_value={"compatible": False}), \
                 mock.patch.object(setup.subprocess, "run") as run, contextlib.redirect_stdout(output):
                self.assertEqual(setup.main(), 1)
            run.assert_not_called()
            result = json.loads(output.getvalue())
            self.assertFalse(result["opencodeRestarted"])
            self.assertFalse(result["collectorStarted"])
            self.assertFalse(result["mcpEnabled"])
            self.assertIn("systemctl --user restart opencode-test.service", result["manualNextSteps"])
            self.assertNotIn("synthetic-only", output.getvalue())


if __name__ == "__main__":
    unittest.main()
