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
bash scripts/install-github.sh --ref economy
# Or pin an already-successful build:
bash scripts/install-github.sh --run RUN_ID
mneme --version
```

The default branch selection is `main`; use `--ref economy` until the workflow
changes are merged. Installation points `~/.local/bin/mneme` to a revision-keyed
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
