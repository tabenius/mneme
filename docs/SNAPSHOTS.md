# Index snapshots

Mneme's index is a live SQLite WAL database. Copying `index.sqlite`, `-wal` and
`-shm` file-by-file can capture three different points in time, so Mneme is the
only component that exports its own index. Backups and restores go through the
`snapshot` owner commands; other tools call them instead of touching the files.

```sh
mneme --db /private/index.sqlite snapshot create --out /private/mneme-2026-11-14.sqlite
mneme snapshot verify /private/mneme-2026-11-14.sqlite --sha256 HEX
mneme --db /private/new-index.sqlite snapshot restore --from /private/mneme-2026-11-14.sqlite --sha256 HEX
```

All three commands print one JSON object to stdout. Diagnostics go to stderr.
`create` and `verify` are read-only with respect to the live index; `restore`
never modifies the snapshot it reads.

## Owner contract (`mneme.snapshot.v1`)

`create` and `verify` emit the same manifest fields:

| Field | Meaning |
| --- | --- |
| `schema` | Always `mneme.snapshot.v1`. Consumers must reject any other value. |
| `format` | `sqlite3`. |
| `mneme_version` | The binary that produced the verdict, from `CARGO_PKG_VERSION`. |
| `index_user_version` | The SQLite `user_version` of the snapshot. Currently exactly `2`. |
| `generation` | The source index's `meta.generation`. |
| `bytes`, `sha256` | Size and lowercase SHA-256 of the published file. |
| `integrity` | `ok`; any other value is a failure. |
| `counts` | `sessions`, `hidden_sessions`, `messages`, `tool_runs`, `raw_events`, `checkpoints`. |
| `sources` | `source`, `harness`, `version` only; upstream error text and URLs are omitted. |
| `sensitivity` | Fixed notice: private plaintext transcripts, owner-only, encrypt off-machine. |
| `coverage` | Snapshot semantics: one consistent read transaction, not a complete archive. |
| `created_at_ms`, `path` | Added by `create` after publication only. |
| `restored_to` | Added by `restore` only. |

Exit status is `0` only when the whole command succeeds. A consumer must treat a
non-zero exit, non-JSON output, a foreign `schema`, or `integrity != "ok"` as a
failed snapshot. The digest and byte count are the consumer's independent handle
for the file it received; callers should re-hash the file rather than trusting the
manifest alone.

## What `create` guarantees

1. Refuses to overwrite an existing target.
2. Opens the live index read-only and verifies that its `user_version` and schema
   objects match this binary before reading it.
3. Runs `VACUUM INTO` in one read transaction, so the collector may keep writing.
4. Creates the destination owner-only (`0600` on Unix) from its first byte.
5. Requires the copy to be a self-contained rollback-journal database with no
   `-wal`, `-shm` or `-journal` sidecar.
6. Runs the full `verify` checks below on the temporary file.
7. `fsync`s the file, publishes it with a hard link that cannot replace a
   concurrent file, and `fsync`s the parent directory.
8. Never exposes a partial file under the final name.

An unchanged index re-exports byte-identically, so content-addressed backup
layers can reuse chunks across snapshots.

## What `verify` guarantees

`verify` is read-only and leaves the snapshot bytes and mtime unchanged. It
rejects a snapshot unless all of the following hold:

- The path is a regular file, not a symlink or special file.
- No `-wal`, `-shm` or `-journal` sidecar exists next to it.
- The SQLite header is present and uses rollback-journal format, not WAL.
- The file digest matches `--sha256` when one is supplied.
- `PRAGMA integrity_check(20)` returns only `ok`.
- `PRAGMA foreign_key_check` reports no violations.
- `user_version` is exactly the version this binary creates.
- Every user-visible table, index, trigger and view matches the schema built from
  the binary's own `SCHEMA` definition, including its `SQL` text. SQLite-internal
  and FTS5 shadow objects are excluded because their representation can vary
  between SQLite builds.
- Every required table exists.
- The FTS5 document count equals the message count, no indexed rowid is missing
  a message, no message is missing from the index, and the FTS configuration is
  present.
- The file opened for hashing is still the same inode (device, inode, size and
  mtime on Unix) at the end of verification, so a concurrent replacement cannot
  slip different bytes past the checks.

The verification connection is opened read-only with `query_only=ON`,
`trusted_schema=OFF` and `defensive=ON` (best effort). A snapshot's embedded
schema is untrusted input; it is never allowed to execute functions or write.

## `restore`

`restore` verifies first, then copies to a new owner-only temporary file and
re-checks the digest while copying. It refuses to create an index when the target
file, `-wal` or `-shm` already exists. The new file is hard-linked into place,
`init` re-enters normal WAL operation and schema guards, and a failure during
`init` removes the half-initialized index so a retry is not blocked.

Restore is deliberately create-only. It is not an in-place downgrade, upgrade or
merge tool.

## Rebsync integration

[rebsync](https://github.com/tabenius/rebsync)'s Mneme layer calls this contract
directly:

1. `mneme snapshot create --out <temp>` returns the manifest.
2. Rebsync re-hashes the file itself and checks `schema` and `integrity`.
3. The file is split into fixed-size content-addressed chunks published under
   `mneme/v1/chunks/sha256/<aa>/<sha256>`, followed by an immutable manifest under
   `mneme/v1/snapshots/<label>/<stamp>-<sha12>.json`.
4. Restore reassembles the chunks, checks every chunk and the whole-file digest,
   then calls `mneme snapshot verify` and finally `mneme snapshot restore`.
5. With `mneme.restic = true` the verified export and manifest are also covered
   by the encrypted restic repository.

Rebsync never claims a snapshot is sound on its own authority; it asks Mneme to
validate it. The `--sha256` argument is how the caller binds the bytes it has to
the owner's verdict.

## Compatibility policy

- `user_version` is the index format. This project has only ever created
  `user_version = 2`; there is no released `1`. Verification therefore requires an
  exact match rather than a range, and `create` refuses to export an index from a
  different schema.
- Any schema change must bump `user_version`, update `INDEX_USER_VERSION`, and
  ship restore/verify support before snapshots of the new format are published.
  Old binaries reject newer snapshots with an explanatory message instead of
  attempting a partial restore.
- `mneme.snapshot.v1` fields may gain optional additions, but existing field names
  and meanings are stable. Consumers must ignore unknown fields.
- Snapshot files are not expected to be byte-compatible across SQLite library
  versions; they are verified with the reading binary's own schema and SQLite.

## Threat model

Snapshots are private plaintext data. The checks defend against:

- accidental truncation, corruption, partial writes and interrupted copies;
- a WAL-mode or multi-file copy that is not self-contained;
- a foreign SQLite database, a schema from another Mneme version, or a snapshot
  with injected tables, indexes or triggers;
- an FTS index that no longer matches the message table;
- concurrent replacement while the file is being hashed or verified;
- unsafe functions or writes triggered by an untrusted embedded schema.

The checks do **not** defend against a snapshot that was maliciously built to have
a valid schema, matching counts and a self-consistent digest. Treat a snapshot as
you would the database it contains: keep it owner-only and encrypt it before it
leaves the machine. `restore` does not authenticate provenance; bind the expected
digest from a trusted source with `--sha256`.
