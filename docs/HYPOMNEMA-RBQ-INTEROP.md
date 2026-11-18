# Hypomnema ↔ rbq-sync ↔ Dash: recovery interoperability gate (2026-11-18)

Status: **proposed integration contract**, not a declaration of live deployment.
This document is intentionally narrower than continuous unattended synchronization.

## Existing contracts (do not conflate)

| Owner | Durable unit | Transport/access | Security boundary |
|---|---|---|---|
| Mneme | SQLite index snapshot, owner verified via `mneme snapshot verify` | rbq Mneme layer | Private plaintext; encrypt when transported off host |
| Mneme Hypomnema | `hypomnema.bundle.v1` gzip of explicit MVH JSONs | Explicit `mneme evac push` | No ambient scans or implicit upload |
| rbq-sync / Rebeqah | Immutable, content-addressed layer snapshots and Git bundles | Verified local and peer transfer | Checksums are integrity, not authenticity |
| Dash evacuation | Instance-scoped immutable bundle ID and R2 object | Connector push + scoped read MCP | Distinct push and read credentials |
| Dash personal handoffs | Account/harness/source/session revision heads | Personal handoff MCP | Separate from instance-scoped evacuation |
| Rebekah | `workspace-handoff.schema.json` + OCI evidence references | OCI artifact workflow | Explicit evidence kind/digest/source |

### Deployment blocker

On 2026-11-18, Dash `main` does **not** contain the Hypomnema sink or personal handoff code. It is on `feat/hypomnema-release`, which diverged from `main` (two ahead, 121 behind when checked). Do not claim remote availability from repository history alone. Reconcile/cherry-pick into a new branch from current Dash `main`, run the migration/tests, then perform an authenticated live canary with synthetic content and a scoped reader. Do not merge the stale feature branch indiscriminately.

The prototype's `get_mvh` returns **metadata only** and `verify_bundle` checks the **blob hash only**; a full recovery reader needs bounded gzip decode + schema + item digest validation.

## Required cooperation protocol

1. **Capture.** Mneme collectors continue local OpenCode indexing. An agent/human writes a bounded explicit MVH, with `state` as an attributed claim and `verify` as unexecuted commands. No automatic export from an indexed transcript.
2. **Stage.** Build an immutable Hypomnema gzip using explicit inputs. Preserve `bundle_id` and SHA-256; never rewrite or silently reuse an output filename. Keep the private staging directory owner-only.
3. **Preserve offline.** rbq-sync transfers the already-reviewed gzip as an *opaque named file*, with content hash, snapshot provenance and retention policy. Never pretend the rbq Mneme index-snapshot layer and a small Hypomnema handoff are the same artifact.
4. **Publish online (opt in).** The owner explicitly runs `mneme evac push` with the dedicated connector credential, only after review. A successful HTTP response means accepted, not proven durable or recoverable.
5. **Recover.** Reader authenticates to Dash using a **separate scoped MCP credential**. Verify source/instance authorization *before* R2 access; perform bounded decompression, full MVH schema/identity validation and item hashes locally. Show each assertion as an untrusted claim. No shell execution of `verify` or `next_action`.
6. **Reconcile.** Match on `bundle_id` (content-derived identity) and optional project/worktree lineage. Never use personal handoff revision-head identity as a replacement for `bundle_id`. If both exist, show two linked records, not silently merged text.
7. **Erase.** Erasure in Mneme, rbq snapshots, Dash R2/D1 and personal handoffs is *separate*. Local `rbq ... prune --apply` does not erase peer copies; Dash has no proven full remote erasure workflow. Document outstanding copies rather than assert global deletion.

## Near-term acceptance gates

- **Dash main reconciliation:** end-to-end synthetic push, list, download/get MVH, full schema/hash verify, wrong-instance denial, revoked-token denial; deployment smoke on `dash.ragbaz.cc`.
- **Opaque rbq artifact adapter:** configured explicit Hypomnema staging directory, no default ambient discovery; hash round-trip from laptop → phone → second laptop with failed/damaged/missing chunk cases. Consent gate for network transmission is separate from rbq peer sync configuration.
- **Recovery CLI:** `mneme evac inspect/verify` on a local gzip, bounded before allocation and with no network calls or index writes. Treat imported `verify` strings and refs as data.
- **Provenance:** record producer version, source Git commit, observed timestamp and independent verification results *outside* MVH v1 until a versioned schema is agreed; never bump caller-supplied `last_verified_at`.
- **Lifecycle:** explicit retention/erasure across local index, offline peers, Dash blobs, handoff history and any backups; restoration tests including compromised/stale handoffs.
- **Continuous staging (later):** only from an allowlisted, human-reviewed handoff directory with a dry-run diff, debounce, max age, idempotent ID and no automatic external push. No general worktree or session sweep.

## Useful links

- Mneme: `docs/HYPOMNEMA-LOCAL.md`, `schemas/mvh.schema.json`, `tests/evac.rs`
- rbq-sync: `README.md` (Mneme layer, fsck, peer and prune)
- Dash feature branch: `docs/HYPOMNEMA.md`, `docs/HANDOFF-MCP.md`
- Rebekah: `docs/contracts/workspace-handoff.schema.json`, `tools/oci_handoff.py`

### Non-goals of this document

It does not install a scheduler, collect sessions, authorize credentials, deploy Dash, prove CI success, or certify that the old branch is safe to merge.
