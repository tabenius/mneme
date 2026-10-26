# Hypomnema: local producer milestone

Implemented: `mneme evac build`, using **explicit handoff JSON files only**,
and opt-in `mneme evac push`. Continuous staging and worktree scanning in
[the original plan](HYPOMNEMA.md) remain unimplemented. Build involves no upload,
credentials, session/index reads, or automatic private-data discovery.
Existing output is never overwritten; choose a new output filename.

```sh
mneme evac build --input ./handoff-a.json ./handoff-b.json \
  --output ./evac.bundle.json.gz --observed-at 1793000000
```

The output parent directory must already exist. `--observed-at` is an explicit
caller-selected Unix-seconds observation time (not a verification time);
`--name` defaults to `evac`. Names accept 1–128 ASCII letters, digits, dots,
underscores or hyphens. Project keys accept 1–64 of those characters plus `/`.
Project keys are inert identifiers, never filesystem paths. Duplicate keys fail.
The command prints only bundle metadata and a SHA-256 of the gzip bytes.
On Unix the bundle is private (0600), staged in its destination directory and
published without replacing existing files or symlinks. Input symlinks and
non-regular files are rejected. Use a trusted local directory: this is not a
sandbox against concurrent hostile filesystem changes. No encryption is applied;
explicit inputs may still contain secrets. Review them before copying the bundle.

## Input contract

[MVH JSON Schema](../schemas/mvh.schema.json) documents the contract; Rust typed
deserialization and field checks enforce it without a schema engine. All fields
are required, unknown fields rejected. Example (synthetic, not host state):

```json
{
  "project": "example",
  "worktree": "/synthetic/example",
  "goal": "Resume the local producer",
  "state": "Claim: implementation drafted; tests not yet checked",
  "open_files": ["src/evac.rs"],
  "decisions": ["Explicit inputs only"],
  "blockers": [],
  "next_action": "Run cargo test",
  "verify": ["cargo test"],
  "refs": ["synthetic:handoff-author"],
  "last_verified_at": 0
}
```

`state`, decisions, verification times, and references are **caller claims**.
`verify` contains proposed verification commands, never executed by this tool.
`last_verified_at` is a caller-supplied Unix-seconds timestamp; zero means none
supplied. It is preserved, not advanced or vouched for. Paths and references are
inert strings and are never followed. Hashes establish byte integrity only, not
authenticity or truth. The producer does not independently verify project state.

Limits: 1–32 inputs, each at most 256 KiB, at most 2 MiB total raw input,
each text field/entry at most 16 KiB UTF-8 and nonblank, at most 128 entries
per list. Parsing has serde_json's default nesting-depth limit. Raw size checks
use bounded reads, including for files that grow. Final gzip output must be at
most 256 KiB; larger output fails before publication. There is no recovery/import
command.

## Gzip JSON interoperability and integrity

The envelope retains `hypomnema.bundle.v1` and the `items` / `mvhs` layout from
the plan. `mvhs` is keyed by project; items are sorted by project key. Each
`mvh_hash` is lowercase SHA-256 hex of that MVH's compact JSON with recursively
lexicographically sorted object keys, UTF-8, and no trailing newline;
`size_bytes` is the length of those same uncompressed bytes. This is serde_json
serialization, **not** a general RFC 8785/JCS implementation.

`bundle_id` is the full lowercase 64-character SHA-256 hex digest (no name prefix),
compatible with Dash's 2–64 ASCII `[A-Za-z0-9._-]` identifier contract.
The digest covers the compact sorted-key
envelope with `bundle_id` set to the empty string. Unlike the draft's illustrative
wall-clock ID, it commits to the complete payload. The final compact envelope is
gzip-compressed with mtime zero, OS 255, no filename/comment, and default flate2
compression. Identical validated input, name, and observation time produces
identical bytes with the same encoder version, independent of input order and
formatting. Cross-version compressor byte identity is not promised. The stdout
digest covers the **final gzip bytes**, distinct from MVH and bundle-ID digests.
There is no signature or live remote receiver test. Receivers must independently bound decompression, validate
schema and hashes, and treat every handoff as untrusted text.

Synthetic tests cover deterministic bytes, envelope and item hashes, schema and
size rejection, preservation of claims, private permissions, no-clobber behavior,
and a CLI invocation that creates no index. Synthetic loopback HTTP tests verify
the push envelope, bearer header, gzip bytes, status handling, redirect rejection,
endpoint restrictions and size limits. Continuous refresh, scheduling,
input discovery, provenance acquisition, and recovery/import are deferred.

## Explicit push

Review bundle contents before choosing to transmit them. Nothing pushes implicitly.

```sh
mneme evac push --bundle ./evac.bundle.json.gz \
  --credential-file ./explicit-push-token
```

The credential file contains a **plain bearer token**, optionally with a trailing
newline (not JSON). It must be a regular non-symlink file, at most 4096 bytes.
No default credential path or environment-variable fallback exists. Keep it
private (0600); the command does not print tokens, submitted contents, or response
bodies. Upstream session collection remains GET-only; this explicit Dash connector
POST is separate from upstream collection and is not exposed through MCP.

The default destination is exactly
`https://dash.ragbaz.cc/api/connector/hypomnema/push`. `--endpoint` may select
HTTP(S) literal loopback IPs for synthetic tests, with that same path. Other
destinations, URL credentials, queries, fragments, redirects and ambient proxies
are rejected/disabled. No retries; connection timeout is 5 seconds and total
request timeout 30 seconds. Only 2xx responses count as acceptance (not independent
proof of remote persistence). An ambiguous transport failure may have delivered
the bundle; its content-derived ID remains stable for a deliberate retry.

Push reads at most 256 KiB gzip, bounds decompression to 3 MiB, and revalidates
the schema, bundle ID, handoffs, and item hashes/metadata before reading credentials.
The JSON POST is at most 512 KiB and has exactly these fields:
`{schema,bundle_id,name,observed_at,items,data}`, where `data` is standard padded
base64 of the unchanged gzip bytes. Large item metadata can exceed the request
limit even when gzip is small; this fails before any network request.
Success prints only the same bundle metadata/hash summary as build.
