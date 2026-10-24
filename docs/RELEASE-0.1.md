# Mneme 0.1 foundation summary

## Delivered

- Rust GET-only OpenCode V2 adapter with loopback-only authentication and contract checks.
- Cursor-paginated historical snapshots; available durable log events stored separately.
- Global notifications, up to eight recent-session followers, reconnect/backoff and reconciliation.
- Process-exclusive collector; atomic snapshots/checkpoints; content-hash deduplication.
- SQLite WAL + FTS5 index, source-qualified identities and explicit removal handling.
- Eight read-only MCP tools with native MCP negotiation, structured results and safety annotations.
- Filter-bound, generation-checked pagination; bounded message chunks and SQLite query deadlines.
- CLI, revision-addressed local installer, dedicated headless user-service template, CI.

No model inference, source-database modification, cloud transcript upload or
OpenCode restart is required. The companion MCP server can be registered in a
running OpenCode instance using its own preserving CLI.

## Verification

Fifteen synthetic tests cover ordered retrieval, keyset pagination, FTS/literal
filtering, all tools, tool-state aggregation, transactional rollback, idempotent
replay, unknown event retention, removal, parameterization, source identity,
long-message chunking, cursor expiry, process locking, HTTP pagination,
resumable SSE and notification EOF recovery.

Additional release gates:

- `cargo fmt --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- Actual stdio initialization, tool listing and calls to all eight tools through `scripts/smoke-mcp.py`.
- Live API backfill, connected MCP registration and fresh collector health.
- End-to-end bounded-query timings through `scripts/benchmark.py`.

## Deliberate limitations

- Only OpenCode V2 in this foundation; no silent fallback to other harnesses.
- Projected transcript snapshots are not complete historical event or diagnostic logs.
- Experimental replay availability depends on the server. Historical reads may
  contain only synchronization markers; no missing events are invented.
- Followers are notification sources; correctness is based on snapshot/log
  reconciliation, not a promise of exactly-once live delivery.
- Searchable message text excludes reasoning, attachments and tool bodies;
  tool input/output are separately searchable in literal mode.
- Model/agent filters and groupings use session metadata, not independently
  reconstructed per-request attribution.
- Removal hides data but does not physically erase retained local records.
- A changing index expires pagination cursors; clients restart the query.
- Endpoint rediscovery, additional harness adapters, ranked semantic search,
  richer usage analytics and full tool-output pagination remain P1.

The core principle remains: **a historical statement is evidence that something
was recorded, not proof that it happened or succeeded.**
