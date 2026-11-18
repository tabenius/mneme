# Mneme 0.2.1 release notes

## Snapshot owner contract hardened

Mneme owns its live SQLite WAL index, and backup tools must ask it for a
consistent export rather than copy the files. The `mneme.snapshot.v1`
`create`/`verify`/`restore` contract that
[rebsync](https://github.com/tabenius/rebsync) consumes is now defensive on both
sides of the hand-off.

- `create` checks the source index's `user_version` and complete schema before
  reading it, sets `trusted_schema=OFF`, and refuses to export an index this
  binary does not own.
- `verify` requires the exact current index version, an exact schema-object and
  SQL fingerprint derived from the binary's own `SCHEMA`, `integrity_check`,
  `foreign_key_check`, and FTS document/rowid/config parity.
- `verify` rejects any `-wal`, `-shm` or `-journal` sidecar, a WAL-mode header, an
  unexpected file type, and a file that is replaced between opening, hashing and
  the end of verification.
- Snapshot connections are read-only with `query_only=ON`,
  `trusted_schema=OFF` and best-effort `defensive=ON`; an embedded schema is
  treated as untrusted input.
- `restore` reports initialization context and removes a half-initialized index
  on failure, so the create-only retry path stays usable.
- An unchanged index still re-exports byte-identically, preserving rebsync's
  content-addressed chunk reuse.

The owner contract, compatibility policy and threat model are documented in
[`docs/SNAPSHOTS.md`](SNAPSHOTS.md).

## Retention

Journalctl-style retention bounds index age and size without stopping
collection:

```sh
mneme --db PATH prune --older-than 60d --max-size 1G --apply
mneme collect --follow --retain-time 60d --max-size 1G ...
```

`MNEME_RETAIN_TIME` and `MNEME_MAX_SIZE` provide the same bounds through the
environment. Age and size apply together: age is a guaranteed floor and size an
additional cap. Each applied prune records a per-source `retention_before`
watermark so a backfill does not re-ingest pruned history. Cleanup shares the
resumable helpers used by physical erasure, and the semantic model can be
rebuilt or cleared explicitly.

## Verification

Release gates:

- `cargo fmt --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked`
- Python unittests
- Synthetic retrieval regression gate
- Live retention dry run and collector health check

New archive tests cover byte-identical re-export, read-only verification,
schema drift, injected objects and redefined triggers, unknown index versions,
foreign-key violations, FTS parity, sidecar rejection, unowned source indexes
and the rebsync owner-contract fields.

## Deliberate limitations

- A maliciously built snapshot with a valid schema, consistent data and a
  matching digest is not authenticated. Bind the expected digest from a trusted
  source with `--sha256`.
- Restore is create-only; it is not an in-place upgrade, downgrade or merge.
- Snapshots remain private plaintext and must be encrypted before leaving the
  machine.
- Schema changes must bump `user_version`; older binaries reject newer snapshots
  instead of attempting a partial restore.
