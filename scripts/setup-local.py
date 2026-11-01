#!/usr/bin/env python3
"""Configure Mneme; never restart, upgrade, or stop OpenCode."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone

spec = importlib.util.spec_from_file_location("mneme_preflight", Path(__file__).with_name("check-opencode.py"))
preflight = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preflight)


def atomic_write(path, content, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".mneme-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(content)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def configure(home, binary, db, url, password, compatible):
    """Pure local configuration. No service control or transcript access."""
    config_dir = home / ".config/mneme"
    config_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    config_dir.chmod(0o700)
    db.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    # Reject JSONC rather than rewriting it and destroying comments/other settings.
    opencode_path = home / ".config/opencode/opencode.json"
    if not opencode_path.exists() and opencode_path.with_suffix(".jsonc").exists():
        raise ValueError("Existing opencode.jsonc requires the installed preserving MCP CLI; refusing to shadow it")
    original = opencode_path.read_text() if opencode_path.exists() else None
    config = json.loads(original) if original else {"$schema": "https://opencode.ai/config.json"}
    mcp = config.setdefault("mcp", {})
    # Preserve whichever configuration generation the owner already uses.
    servers = mcp["servers"] if "servers" in mcp else mcp
    command = [str(binary), "--db", str(db), "mcp"]
    existing = servers.get("mneme")
    if existing and existing.get("command") != command:
        raise ValueError("A different Mneme MCP entry already exists; refusing to replace it")
    entry = {"type": "local", "command": command}
    if "servers" in mcp:
        entry["disabled"] = not compatible
    else:
        entry["enabled"] = compatible
    servers["mneme"] = entry
    credential = config_dir / "opencode-service.json"
    atomic_write(credential, json.dumps({"password": password}) + "\n")
    env = {"MNEME_DB": str(db), "MNEME_URL": url, "MNEME_PASSWORD_FILE": str(credential)}
    atomic_write(config_dir / "collector.env", "".join(key + "=" + json.dumps(value) + "\n" for key, value in env.items()))
    updated = json.dumps(config, indent=2) + "\n"
    changed = original != updated
    if changed:
        # Avoid overwriting a concurrent edit made after reading the config.
        if opencode_path.exists() and opencode_path.read_text() != original:
            raise ValueError("OpenCode configuration changed concurrently; retry setup")
        if original is not None:
            stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
            atomic_write(opencode_path.with_name("opencode.json.mneme-backup-" + stamp), original)
        atomic_write(opencode_path, updated)
    return {"collectorEnvironment": str(config_dir / "collector.env"),
            "mcpConfig": str(opencode_path), "mcpEnabled": compatible,
            "configChanged": changed, "database": str(db)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--password-file", type=Path, required=True)
    parser.add_argument("--password-format", choices=("json", "plain"), default="json")
    parser.add_argument("--db", type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--stage", action="store_true", help="Write disabled MCP/config even if preflight fails; do not start collection")
    parser.add_argument("--start-collector", action="store_true", help="Enable/start only Mneme after successful contract preflight")
    parser.add_argument("--opencode-unit", help="Optional unit name to print in MANUAL restart guidance; never executed")
    args = parser.parse_args()
    os.umask(0o077)
    home = Path.home()
    binary = args.binary or Path(shutil.which("mneme") or home / ".local/bin/mneme")
    # Keep the stable symlink in config, not the resolved revision-specific binary.
    binary = binary.absolute()
    if not binary.is_file():
        raise SystemExit("Install Mneme first; binary not found")
    db = (args.db or Path(os.environ.get("XDG_DATA_HOME", home / ".local/share")) / "mneme/index.sqlite").absolute()
    raw = args.password_file.read_text()
    password = json.loads(raw)["password"] if args.password_format == "json" else raw.rstrip("\r\n")
    if not isinstance(password, str) or not password:
        raise SystemExit("Credential file needs a nonempty password")
    result = preflight.inspect(args.url, password)
    if result["compatible"] or args.stage:
        result.update(configure(home, binary, db, args.url, password, result["compatible"]))
    result["collectorStarted"] = False
    if args.start_collector and result["compatible"]:
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        subprocess.run(["systemctl", "--user", "enable", "--now", "mneme-collector.service"], check=True)
        result["collectorStarted"] = True
    result["opencodeRestarted"] = False
    result["manualNextSteps"] = [
        "After MCP is enabled, save work and quit/relaunch the OpenCode TUI to load its configuration.",
        "For a managed web/server instance, restart only its known unit when you choose to do so.",
    ]
    if args.opencode_unit:
        if not args.opencode_unit.endswith(".service") or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.@:-" for c in args.opencode_unit):
            raise SystemExit("Invalid service unit name")
        result["manualNextSteps"].append("systemctl --user restart " + args.opencode_unit)
    if not result["compatible"]:
        result["manualNextSteps"].insert(0, "Use an OpenCode instance providing the required V2 GET endpoints, then rerun setup without --stage. No upgrade or restart has been performed.")
    print(json.dumps(result, indent=2))
    return 0 if result["compatible"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
