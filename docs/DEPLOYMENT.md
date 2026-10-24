# Local deployment

Source, private index and service configuration are separate. No transcript or
credential belongs in Git. The installed release path is keyed by Git revision.

1. Pass tests, commit and push.
2. Run `bash scripts/install-local.sh` to build/install that committed revision.
3. Create `~/.config/mneme/collector.env` with mode 0600, containing:

   ```ini
   MNEME_DB=/absolute/private/directory/index.sqlite
   MNEME_URL=http://127.0.0.1:PORT
   MNEME_PASSWORD_FILE=/absolute/path/to/opencode/service.json
   ```

4. Run `systemctl --user daemon-reload` and `systemctl --user enable --now mneme-collector.service`.
   This starts only a headless collector; it must not start or restart OpenCode.
5. Verify `systemctl --user is-active mneme-collector.service` and `mneme --db PATH status`.
6. Exercise `scripts/smoke-mcp.py` against the installed binary/index.
7. Register with OpenCode's preserving CLI:

   ```sh
   opencode mcp add mneme --global -- /absolute/path/to/mneme --db /absolute/path/to/index.sqlite mcp
   opencode mcp list
   ```

On upgrade, install the new revision then restart **only** Mneme's known service.
Before doing so, inspect its `MainPID` and process arguments. Never signal processes
by a broad program-name pattern. Other services and the current conversation must
remain untouched. The unit uses `KillMode=process` and graceful SIGINT.

Rollback: select a previously installed compatible release symlink and restart
only Mneme. Back up the SQLite database through SQLite's backup API before a
schema upgrade; copying only a live main file omits WAL state. Version 0.1 reads
schema version 2 and upgrades the initial local schema-1 foundation in place.

Removing the MCP config and disabling this exact unit removes integration;
private index data remains until explicitly deleted by the owner.
