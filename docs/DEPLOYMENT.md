# Local deployment

## GitHub-built binaries and Debian packages

The `check` GitHub Actions workflow validates Rust/Python first, then produces
native x86_64 and aarch64 **static musl** binaries, `.tar.gz` installation bundles,
`.deb` packages (`amd64`/`arm64`) and SHA-256 checksums. Pushes and manual runs
build packages; PRs run checks only. Artifacts are retained for 30 days. A pushed
`vVERSION` tag matching `Cargo.toml` additionally publishes a GitHub Release.
Ordinary branch builds use `VERSION+git.COMMIT` package versions.

No Rust compiler is needed on the installation host. From a checkout:

```sh
# Uses gh authentication for this private repository and verifies checksums/revision.
bash scripts/install-github.sh --ref main
# Or pin an already-successful build:
bash scripts/install-github.sh --run RUN_ID
mneme --version
```

The default branch selection is `main`; use `--ref BRANCH` only when deliberately
installing another branch's successful build. Installation points `~/.local/bin/mneme` to a revision-keyed
release and installs user service templates without starting/restarting services.
Add `~/.local/bin` to PATH if necessary. This does not register MCP or collect data.

For direct artifact/package installation:

```sh
gh run download RUN_ID --repo tabenius/mneme --name mneme-x86_64-unknown-linux-musl --dir mneme-download
cd mneme-download
sha256sum --check SHA256SUMS-x86_64-unknown-linux-musl
# User-local, no root:
tar -xzf mneme-*-x86_64-unknown-linux-musl.tar.gz
bash mneme-*-x86_64-unknown-linux-musl/install.sh
# Alternative: system-managed Debian installation, not an additional required step:
sudo apt install ./mneme_*_amd64.deb
```

Choose `aarch64-unknown-linux-musl`/`arm64` on ARM Linux. Releases provide the same
assets via `gh release download --repo tabenius/mneme --pattern ...` after a version
tag is published. Check matching checksum files before extracting/installing.

Debian installs `/usr/bin/mneme`, documentation, and user service templates under
`/usr/lib/systemd/user`, with `/usr/bin/mneme` ExecStart paths. It contains no
maintainer script that starts services. Avoid enabling both a pre-existing
user-local unit and a system-provided unit unintentionally: user-local units take
precedence. Private indexes/configuration stay in the user's existing directories
and are not package payloads. Optional semantic commands still need an appropriate
Python environment (`MNEME_PYTHON`); NumPy is not bundled into the Rust binary.

### Building packages in CI

`scripts/package-release.sh BINARY TARGET OUTPUT_DIR` packages an already-built
binary without network, collection or service access. The existing
`scripts/install-local.sh` remains a source-build alternative. GitHub caches Cargo
downloads/outputs, and the native musl matrix avoids slow emulated compilation.

## Source installation and collector setup

### Configure without restarting OpenCode

The installed OpenCode CLI and running API must be checked separately. Mneme's
current collector requires V2 GET paths (`/api/info`, `/api/session`, projected
messages, experimental session log, and events); OpenCode 1.x is not silently
treated as compatible. A restart alone cannot add a missing API adapter.

```sh
# Prints only version/endpoint compatibility, never the credential or sessions.
python3 scripts/check-opencode.py --url http://127.0.0.1:PORT --password-file /path/to/service.json
# For a plain-text password file instead of JSON, add --password-format plain.

# Configure MCP and start ONLY Mneme after a successful API preflight:
python3 scripts/setup-local.py --url http://127.0.0.1:PORT \
  --password-file /path/to/service.json --start-collector
```

Setup writes private Mneme configuration and preserves existing JSON OpenCode
settings, backing up changed configurations. Existing JSONC is not overwritten or
shadowed. On an incompatible server, no configuration is changed unless `--stage`
is supplied; staging writes a **disabled** MCP entry and never starts collection.
Do not describe this staged state as working ingestion.

**OpenCode is never upgraded, stopped, or restarted by these scripts.** For the
new MCP configuration to load, save work and manually quit/relaunch your OpenCode
TUI. If using a systemd-managed web/server instance, manually restart only its
known unit when ready, e.g. `systemctl --user restart rebekah-opencode-web.service`
on the original Chromebook. `--opencode-unit NAME.service` merely prints that
manual command; it does not execute it. Interactive callers may ask for explicit
confirmation, but must not assume consent or restart by default.

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

## Failure recovery

- MCP negotiation and tool discovery work before the first index exists. Data
  calls return tool errors until collection creates a readable index. The same
  MCP connection recovers on the next call after the index is repaired; it never
  creates an empty replacement or deletes a damaged index.
- The continuous collector retries initial API verification with exponential
  backoff (1–30 seconds), marking source health degraded. An incompatible API
  remains degraded; retries do not add support for older OpenCode contracts.
  Single-pass collection still fails immediately with a nonzero exit status.
- SIGINT and SIGTERM shut down the collector. SIGKILL/process crashes release
  the OS-held writer lock; a remaining `.lockfile` is normal and need not be
  removed. Restart only `mneme-collector.service` after a failure.
- SQLite uses WAL with full synchronization. Schema initialization is atomic;
  snapshots, event records and checkpoints commit together. Do not remove WAL
  files or copy only the main database while a writer is active.
- Upstream reads time out after 60 seconds without bytes; finite requests have
  a 120-second total deadline. Individual JSON/log responses are capped at
  32 MiB. Notification streams rotate at that byte budget and reconcile again.
  These failures retain checkpoints for retry. Invalid continuation cursors
  fail collection rather than treating a partial inventory as complete.
- MCP admits at most four concurrent query workers. Excess calls receive a
  retryable tool error. Semantic helpers have a 15-second deadline including
  input transfer, bounded output, and are killed/reaped on failure.

Check `mneme status` for source errors, reconciliation age and `stale` before
relying on query coverage. A successful MCP handshake alone does not establish
that collection is running or current.

## Optional semantic layer (0.2)

Install numerical requirements in a suitable local Python environment and set
`MNEME_PYTHON` to that interpreter. Set `MNEME_KNOWLEDGE` to an owner-only local
knowledge file if private entity aliases/assertions are needed. Pass those same
environment variables when registering the MCP command with `opencode mcp add
--env KEY=value`; a collector environment file does not configure the MCP client.

Build the initial model with `mneme --db PATH semantic-build`. After checking it,
enable `mneme-semantic-refresh.timer` for hourly refreshes. Both semantic unit
templates are installed without starting them by `scripts/install-local.sh`.
The helper performs local numerical inference only. Do not publish the model
manifest/vocabulary or frozen evaluation fixtures. See `docs/SEMANTIC.md`.
