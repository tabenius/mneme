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
- Ten read-only stdio MCP tools, also callable from a JSON CLI.
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
mneme query history_aggregate --args '{"group_by":"day"}'
mneme status
```

Shared filters include `source`, `session_id`, `directory` (exact), `project_id`,
`agent`, `model`, UTC Unix-millisecond `since_ms`/`until_ms`, and `limit` (1–100).
Message queries support `role` (native message type), `message_id`, `text_offset`
and `text_limit` (1–16000 characters) to retrieve longer text in bounded chunks.
The default excerpt is 2000 characters. Tool input/output excerpts are limited
to 2000 characters each in P0. Hidden reasoning and attachment bodies are not
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
| `history_get_session` | Metadata, parent, upstream-reported usage |
| `history_get_messages` | Paginated projected transcript and text chunks |
| `history_get_activity` | Cross-session chronological evidence packet |
| `history_get_tool_runs` | Recorded tool input, status and output excerpts |
| `history_aggregate` | Typed grouping by project/model/agent/role/tool/status/day |
| `history_index_status` | Counts, versions, freshness and limitations |
| `history_explain_query` | Inspect aliases, entity links and resolved temporal bounds |
| `history_get_knowledge` | Curated concepts/entities and source-attributed assertions |

Run `mneme --db /path/to/index.sqlite mcp` as a local stdio MCP server. Diagnostics
go to stderr. Tool results carry both text and structured JSON. All tools are
annotated read-only, non-destructive and closed-world. No administration tools
are exposed to conversational clients.

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
session removal hides it from tools but does not physically erase retained local
records; rebuild the index for erasure. Do not use this as a public MCP endpoint.
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
```

Tests use synthetic conversations. Live smoke tests print counts and timings,
not transcript contents. See [P0 acceptance plan](docs/PLAN.md).
