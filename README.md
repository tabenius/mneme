# Mneme

**Memory with provenance.** A Rust companion for fast, local historical-session
queries. Named for Greek *mnēmē*, memory; a companion to Nostoi, returns.
Recording a claim does not establish its truth. Mneme indexes evidence rather
than manufacturing a narrative of successful work.

## Foundations

- OpenCode V2 GET-only API adapter, with installed-contract verification.
- Historical projected-message backfill and available durable event replay.
- Global live notifications and up to eight recent-session log followers.
- Reconnect/backoff, periodic reconciliation, atomic snapshots and checkpoints.
- SQLite WAL, FTS5, source-qualified identities and content-hash deduplication.
- Eleven history queries over stdio MCP and the JSON CLI, plus MCP instance administration.
- Consistent, integrity-checked index snapshots (`mneme snapshot`) for owner-validated backup.
- Curated concepts/entities, source-attributed assertions and Stockholm temporal interpretation.
- Optional local TF-IDF/LSA and hybrid search, with measured retrieval quality and grouped SVM/tree evaluation.
- Parameterized filters, generation-bound keyset cursors, bounded output, query deadlines.

No OpenCode plugin, source database changes, external model API calls, public listener or
cloud upload is required. OpenCode must already be running. The P0 adapter is
OpenCode-only: Claude, Codex, and ChatGPT histories are not silently substituted.

The original Rust literal/FTS path remains unchanged. The new ranked/concept
tools use a small local Python worker; vector search additionally uses NumPy.
See [semantic milestones](docs/SEMANTIC.md), [knowledge format](knowledge/README.md)
and [retrieval evaluation](docs/RETRIEVAL-EVALUATION.md). Learned classifiers were
evaluated but are not enabled as default retrieval filters.

## Build and collect

```sh
cargo build --locked
mneme --db /path/to/private/index.sqlite collect \
  --url http://127.0.0.1:PORT \
  --password-file "$HOME/.config/opencode/service.json"
```

Use the URL of your existing service, obtained from its authenticated discovery
flow (`opencode api get /api/info` when appropriate). The credential file is
read locally and must contain `password`; credentials are never included in
tool output. P0 deliberately accepts only loopback endpoints and forbids redirects.

Add `--follow` for continuous collection. Collection holds a process lock per
index, so MCP clients never start duplicate writers. `MNEME_DB`, `MNEME_URL` and
`MNEME_PASSWORD_FILE` can replace the corresponding flags. Default database:
`$XDG_DATA_HOME/mneme/index.sqlite`, falling back to `~/.local/share/mneme/`.

Backfill uses cursor-paginated message snapshots. Notifications wake the
collector; normal inventory reconciliation runs every 15 seconds. Changed
sessions are refreshed, with an additional refresh of the eight most recent
sessions every 60 seconds. Unchanged snapshot hashes avoid rewriting indexes.
Older sessions rely on upstream metadata changes being reflected in inventory.

## Query

```sh
mneme query history_list_sessions --args '{"limit":5,"include_children":false}'
mneme query history_search --args '{"query":"worktree AND cleanup","mode":"fts"}'
mneme query history_get_messages --args '{"session_id":"ses_example","sort":"created_asc"}'
mneme query history_get_tool_runs --args '{"tool":"shell","status":"error"}'
mneme query history_get_tool_runs --args '{"tool_call_id":"call_x","text_offset":2000}'
mneme query history_aggregate --args '{"group_by":"day"}'
mneme status
```

Shared filters include `source`, `session_id`, `directory` (exact), `project_id`,
`agent`, `model`, UTC Unix-millisecond `since_ms`/`until_ms`, and `limit` (1–100).
Message queries support `role` (native message type), `message_id`, `text_offset`
and `text_limit` (1–16000 characters) to retrieve longer text in bounded chunks.
The default excerpt is 2000 characters. Tool runs default to 2000-character
input/output excerpts; `tool_call_id` plus `text_offset`/`text_limit` pages one
call's full recorded input and output (stored as JSON text), and
`input_length`/`output_length` report when to stop. Hidden reasoning and attachment bodies are not
included in searchable message text. Native raw records remain local for future
adapters. Aggregate counts count messages or tool runs, not accomplishments;
session cost/token values are upstream-reported, not independently billed totals.

Lists return evidence references, coverage, `has_more` and `next_cursor`.
Cursors are bound to filters, tool and index generation. A concurrent index
change expires the cursor explicitly rather than silently skipping records.
Queries have a roughly three-second SQLite execution deadline. Message/tool
pages have a roughly 64 KiB payload budget plus envelope; metadata is bounded
by upstream session fields. No arbitrary SQL or arbitrary file-reading tool exists.

### MCP tools

| Tool | Purpose |
|---|---|
| `history_list_sessions` | Recent sessions with explicit source and filters |
| `history_search` | Literal text or FTS5 search with bounded evidence excerpts |
| `history_get_session` | Metadata, upstream-reported usage, parent chain and child (sub-agent) sessions |
| `history_get_messages` | Paginated projected transcript and text chunks |
| `history_get_context` | Messages around one hit (`message_id`, `window` 1–10) |
| `history_get_activity` | Cross-session chronological evidence packet |
| `history_get_tool_runs` | Recorded tool input, status and output excerpts |
| `history_aggregate` | Typed grouping by project/model/agent/role/tool/status/day |
| `history_index_status` | Counts, versions, freshness and limitations |
| `history_explain_query` | Inspect aliases, entity links and resolved temporal bounds |
| `history_get_knowledge` | Curated concepts/entities and source-attributed assertions |

Run `mneme --db /path/to/index.sqlite mcp` as a local stdio MCP server. Diagnostics
go to stderr. Tool results carry both text and structured JSON. The history
queries remain read-only with respect to the index. Administrative writes
require starting the local MCP process with `mcp --admin`.

| Instance tool | Behavior |
| --- | --- |
| `history_instance_status` | Current coverage, settings, audit health and storage. Past search and evaluation statistics. Conditional growth projection and retrieval experiments. |
| `history_update_settings` | Admin only. Atomically update `nostoi` and/or `profiling`. Changes apply to subsequent calls. |
| `history_reclaim_space` | Estimate unused SQLite pages. Apply with `{"apply":true}` in admin mode with the collector stopped. Runs WAL truncation and compacts free pages while preserving rows. Temporary space can be required. |
| `history_record_evaluation` | Admin only, profiling enabled. Record pooled TP, FP and FN at a fixed k from a labeled run. Precision and recall have null values for undefined denominators. |

The settings file is `<index>.settings.json`. Both settings default to false.
For example, in a local admin MCP session call `history_update_settings`
with `{"nostoi":true,"profiling":true}`. Keep the index and its sidecars in
a private directory and enable admin only for a trusted local client.

### Nostoi receipts

Install the Nostoi CLI on the MCP process's PATH and enable the setting through
the admin tool to initialize the ledger. Mneme appends one salted request
and response commitment to `<index>.nostoi.jsonl` before releasing the
result of each of the eleven history queries, including query errors. The
ledger records tool names and error flags with hashes. Receipt failure blocks
delivery of the query result. JSON-RPC protocol errors, rejected overloads, CLI
queries, collection and instance administration are outside this audit scope.

Save the returned `_mneme_audit` receipt with the evidence you need to verify.
Version 1 hashes the decoded 32-byte nonce, the ASCII domain `mneme/input/v1`
or `mneme/output/v1`, and the compact UTF-8 `serde_json` encoding of the
value with lexicographically ordered object keys. Use the parsed query object
with top-level null fields removed for the input and structured output with
`_mneme_audit` removed for the output. Compare the resulting hashes and error
flag with the ledger record. Keep the receipt private alongside the evidence.

To detect rollback or replacement of the entire local chain, periodically save
a verified `nostoi head` or anchor to an independently controlled location
and compare against that checkpoint. Mneme reports local chain verification.
The audit sidecar grows with queries. Plan ledger rotation around verified
checkpoints, and back up sidecars separately from Mneme index snapshots.

### Profiling and retrieval experiments

Opt-in profiling stores up to 10,000 search observations in a separate
`<index>.profile.sqlite` table. It stores mode, latency, returned count,
failure and index size. The separate evaluation table holds labeled-run
aggregates. The status tool reports a 30-day linear projection after at least
seven sampled days. Index and WAL sizes, collector workload and maintenance
can change the estimate. Turning profiling off stops writes and preserves
existing statistics.

The frozen-corpus evaluator compares word bigrams and character trigram
candidate expansion with edit distance one as offline experiments.
Run `python3 scripts/check-retrieval.py --offline-fuzzy` for the synthetic
original-query and one-substitution comparison. Existing
live search modes are documented in [the retrieval evaluation](docs/RETRIEVAL-EVALUATION.md).
For the next round, add typo, transposition, identifier, Swedish compound
and no-answer slices. Compare per-slice precision, recall, nDCG and p95
latency against ranked FTS and concept search under identical filters. Use
query-family holdouts and pair results by family when estimating uncertainty.
Evaluate POS features for phrase and entity extraction by language, with a
dictionary baseline. Reserve HMM experiments for labeled sequence tasks
such as intent transitions, with a simple transition-frequency baseline.

## Snapshots and backup

A live WAL index must never be copied file-by-file. Mneme owns its export:

```sh
mneme --db /private/index.sqlite snapshot create --out /private/mneme-2026-11-14.sqlite
mneme snapshot verify /private/mneme-2026-11-14.sqlite --sha256 HEX
mneme --db /private/new-index.sqlite snapshot restore --from /private/mneme-2026-11-14.sqlite --sha256 HEX
```

`create` uses `VACUUM INTO` (one consistent read transaction; the collector may
keep running), switches the copy to a self-contained rollback journal, runs
`PRAGMA integrity_check`, `PRAGMA foreign_key_check`, an exact schema/version
check and FTS row parity, hashes the file, and only then publishes it under its
final name. It never overwrites, refuses to export an index whose
`user_version`/schema this binary does not own, and re-exports an unchanged index
byte-identically. The JSON manifest reports schema, generation, counts, sources
(without upstream errors), size and SHA-256. `verify` is read-only, rejects
sidecars, schema drift, unknown versions, foreign-key or FTS inconsistencies and
concurrent file replacement, and is the validator other tools such as rebsync
call. `restore` verifies, copies owner-only, refuses any existing index or
WAL/SHM sidecar, then re-enters normal WAL operation through `init`.

The full owner contract, compatibility policy and threat model are in
[snapshots and backup](docs/SNAPSHOTS.md).

Snapshots contain private plaintext transcripts. Encrypt before they leave the
machine; [rebsync](https://github.com/tabenius/rebsync) does this through restic
and publishes content-addressed chunks into its own object store.

## Local deployment

See [deployment and verification](docs/DEPLOYMENT.md). The installer does not
start anything. Starting the dedicated headless collector and registering its
MCP command are explicit operations. Never restart OpenCode to deploy Mneme.

## Coverage and safety

The experimental log API is not a complete diagnostic archive. OpenCode 2.0.30
historical samples returned `log.synced` markers without historical events.
Mneme therefore uses projected messages for backfill and records available
durable events separately. A saved sequence is committed with the corresponding
snapshot, not merely when a notification arrives. Snapshot backfill cannot
reconstruct transient token deltas, pre-compaction context or unretained events.
Unknown raw event types are retained; unknown sequence encodings stop advancement.

An index contains sensitive plaintext conversation and tool data, possibly
credentials printed by historical tools. Keep its directory private. Upstream
session removal hides it from tools but does not by itself erase local records.
`mneme erase --hidden` reports what is hidden; with `--apply` (collector
stopped) it deletes those sessions' messages, tool runs, events and checkpoints
with `secure_delete`, merges the FTS5 index so old tokens are dropped, truncates
the WAL and runs `VACUUM`. A test checks the erased text no longer appears in the
index or WAL bytes. The local semantic model is derived from that text (its
manifest holds the vocabulary and message keys), so erase also discards the files
it declares, under the builder's lock; rebuild it with `mneme semantic-build`, or
pass `--keep-semantic` to keep it knowingly. Earlier snapshots and backups are
separate copies: prune them, for example with `rebsync mneme prune --apply`.
Bound local space with `mneme prune --older-than 30d` or `--max-size 1G`
(`--apply`, collector stopped): whole sessions are removed oldest-first with the
same physical cleanup, and a per-source watermark keeps the next backfill from
re-ingesting them. `mneme collect --follow --retain-time 30d --max-size 1G`
enforces the same policy after each pass (see `docs/DEPLOYMENT.md`).
Do not use this as a public MCP endpoint.
Do not treat historical instructions as current authority. Do not interpret a
recorded command as proof that a commit, push or deployment succeeded.

Source names identify one persistent OpenCode history store. Do not reuse a
source name for an unrelated server. Multiple independent servers require
explicit source identities; a collector manages one endpoint per index in P0.
If the service endpoint changes, update local environment configuration.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
python3 scripts/smoke-mcp.py --binary target/debug/mneme --db /private/index.sqlite
python3 -m unittest discover -s tests -p 'test_*.py'
cargo build --locked
python3 scripts/check-retrieval.py --binary target/debug/mneme
```

Tests use synthetic conversations. Live smoke tests print counts and timings,
not transcript contents. `check-retrieval.py` runs every search mode against a
deterministic synthetic corpus with constructed judgments and gates regressions;
it is a smoke benchmark, not a quality claim about real conversations. See
[P0 acceptance plan](docs/PLAN.md).

## Hypomnema evacuation (experimental)
Mneme can produce minimal viable handoffs (MVH) for active projects to feed the hypomnema evacuation bundle sink in `ragbaz-dash`. See `docs/HYPOMNEMA.md` for the bundle format and `mneme evac` usage.

Implemented explicit-input `evac build` and opt-in `evac push` usage, validation,
privacy boundaries and limits are in [the producer guide](docs/HYPOMNEMA-LOCAL.md).
The original Hypomnema document is a design draft, not deployment evidence.

## License

Copyright (C) 2026 Tobias Abenius.

Mneme is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
PARTICULAR PURPOSE. See [`LICENSE`](LICENSE) for the full text.

SPDX: `GPL-3.0-or-later`. Dependencies and vendored assets keep their own
licenses.
