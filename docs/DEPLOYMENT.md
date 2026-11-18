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
  fail collection rather than treating a partial inventory as complete, and
  pagination is capped at 1,000 pages / 100,000 rows per inventory.
- An empty upstream session inventory would hide every indexed session, so it
  is rejected by default. Pass `--allow-empty-inventory` only when the source
  genuinely has no sessions left. The same applies to a single-pass
  `mneme collect`.
- Physical erasure commits an `erase_pending` marker before deleting rows. If
  the process stops before the WAL fold/`VACUUM` cleanup, `mneme status`
  reports `cleanup_pending: true` and the index is unhealthy until
  `mneme erase --hidden --apply` is rerun. Model-file removal failures abort
  before any index row is deleted, leaving the manifest intact for a retry.
  `mneme prune --apply` uses the same marker and final cleanup.
- MCP admits at most four concurrent query workers. Excess calls receive a
  retryable tool error. Semantic helpers have a 15-second deadline including
  input transfer, bounded output, and are killed/reaped on failure.

Check `mneme status` for source errors, reconciliation age and `stale` before
relying on query coverage. A successful MCP handshake alone does not establish
that collection is running or current.

### Status and error contract

`mneme status` and `history_index_status` always answer with metadata, even when
no index exists; they never start collection. Branch on these fields instead of
parsing prose:

- `ok`: the index is readable and contains at least one session.
- `healthy`: `ok` and no source is stale or degraded, and no erasure cleanup is
  pending.
- `state`: `ready`, `empty`, `missing` or `unreadable`.
- `index.stale`: no reconciliation within `stale_after_ms` (60 seconds).
- `index.upstream_errors`: collector-reported upstream failures.
- `cleanup_pending`: an erasure's WAL/`VACUUM` cleanup has not completed.
- `remedy`: the operator action, or `null` when healthy.

Every MCP tool error also carries a stable `code` (`index_missing`,
`index_unreadable`, `index_empty`, `cursor_expired`, `invalid_request`,
`query_timeout`, `overloaded`, `query_failed`), a `hint`, `retryable`, and the
same metadata-only `index` diagnosis. Diagnoses never include transcript text and
are safe to log.

### Physically erasing upstream-deleted sessions

Removal only hides sessions until the owner chooses to erase them:

```sh
mneme --db PATH erase --hidden            # dry run: counts and model impact only
mneme --db PATH erase --hidden --apply    # requires the collector to be stopped
```

`--apply` discards the local semantic model before deleting rows because the
model is derived from the erased text (use `--keep-semantic` only if that
exposure is acceptable, then rebuild with `mneme semantic-build` when the
erasure is complete). Snapshots and backups made earlier are not reachable and
must be pruned separately. A dry run opens the index read-only and never creates
or modifies files.

### Bounding index space (retention)

Retention is modelled on `journalctl --vacuum-time` / `--vacuum-size`: it is a
policy, not a hard quota, and it removes whole sessions oldest-first rather than
truncating a session.

```sh
mneme --db PATH prune --older-than 30d              # dry run: what would go
mneme --db PATH prune --older-than 30d --apply      # requires the collector stopped
mneme --db PATH prune --max-size 1G --apply         # keep the newest sessions near 1 GiB
mneme --db PATH prune --older-than 30d --source local-opencode --apply
mneme --db PATH prune --clear-retention --apply     # forget prune watermarks
```

Durations accept `s`, `m`/`min`, `h`, `d`, `w`, `mo`/`month` and `y` (`30d`,
`1w`, `12h`). Sizes accept binary units (`1G`, `1GiB`, `500M`, `2T`) or a plain
byte count. Both bounds may be combined; a session is removed when it is either
too old or the oldest data must go to approach the size target. The size target
is best-effort: if the remaining newest sessions alone exceed it, the report
shows `target_met: false` and a warning instead of claiming success.

Applying a prune runs the same physical cleanup as erasure (`secure_delete`,
FTS merge, WAL truncation, `VACUUM`), so text no longer remains in free pages or
the WAL, and it resumes from the `erase_pending` marker after an interruption. It
also discards the semantic model before deleting rows because that model is
derived from the pruned text; use `--keep-semantic` only if that exposure is
acceptable, then rebuild with `mneme semantic-build`.

Each applied prune records `retention_before:<source>` at the newest removed
update time. The collector skips upstream sessions at or below that watermark,
so the next backfill does not re-ingest pruned history. A session updated
upstream after the watermark is treated as new activity and collected again.
Because the comparison is strict, an upstream session that was never indexed and
shares the exact millisecond boundary timestamp with a pruned session is also
skipped; `--clear-retention` removes the watermarks if that history is wanted
again. Sessions already hidden by upstream deletion are always pruned as well.

The continuous collector accepts retention directly, and enforces it after each
successful pass:

```sh
mneme collect --follow --retain-time 30d --max-size 1G ...
# or set MNEME_RETAIN_TIME=30d and MNEME_MAX_SIZE=1G in the unit environment
```

A retention failure never stops collection; it marks source health degraded and
retries on the next pass. `mneme prune` without `--apply` only reports. Snapshots
and backups made before a prune still contain the removed sessions and must be
pruned separately. Sessions already hidden by upstream deletion are erased by
the next applied prune, regardless of the age or size bound.


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
