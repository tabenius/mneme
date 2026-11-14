# Foundation plan and acceptance

## P0 (release gate)

- [x] Verify installed V2 API routes, authentication, pagination and log behavior.
- [x] Backfill historical sessions/messages through GET-only APIs.
- [x] Follow global/recent-session events, reconnect and reconcile missed notifications.
- [x] Atomically commit snapshots, available event records and native sequence checkpoints.
- [x] Provide all eight documented read-only MCP tools with bounded responses and provenance.
- [x] Test rollback, replay, filtering, source separation, removal and cursor correctness.
- [x] Exercise actual MCP negotiation/tool calls and benchmark representative live queries.
- [x] Commit/push source, deploy a dedicated headless user service, register MCP, verify health.

Foundation release status: complete. See [release summary](RELEASE-0.1.md).
Host-specific inventory and timing measurements belong outside this repository.

## P1 (not claimed as P0)

The first semantic milestones M1–M6 and the exploratory retrieval/SVM/tree
evaluation are implemented in 0.2. See [semantic scope](SEMANTIC.md) and
[measured quality](RETRIEVAL-EVALUATION.md). Further improvements below remain
open; implementation completion is not a claim that retrieval quality is solved.

Structured index diagnostics (`ok`/`healthy`/`state`/`remedy`, stable error codes)
and a committed synthetic retrieval regression gate are implemented. The gate
exercises every search mode in CI but does not replace private-corpus quality
evaluation in [retrieval evaluation](RETRIEVAL-EVALUATION.md).

- Additional harness adapters, starting with Claude Code and Codex.
- Global model-request diagnostics; not equivalent to persisted session events.
- [x] Context-window navigation around a hit (`history_get_context`).
- Branch-aware session comparison.
- [x] Consistent owner-validated index snapshots (`mneme snapshot create|verify|restore`).
- [x] Full tool-output pagination (`tool_call_id` + `text_offset`/`text_limit`).
- Literal-search acceleration if measured necessary.
- Independently verified token/cost analytics, summaries and semantic search.
- General filtered aggregation DSL, top-N ranked relevance and persistent query snapshots.
- Automatic endpoint rediscovery, archival retention/erasure tooling and multi-source collector orchestration.
- Tantivy or Parquet only after workload benchmarks establish a need.

## Evaluation questions (synthetic acceptance corpus)

The integration tests verify these without model inference:

1. Which five OpenCode sessions were most recently active?
2. Can the next page be read without duplicates?
3. Which user messages mention worktree within a date range?
4. Do all eight tools produce structured bounded results?
5. Which shell runs completed, and how many are indexed?
6. Does a malformed snapshot leave the old checkpoint/transcript intact?
7. Does replay preserve unknown events without duplicating records?
8. Are removed sessions excluded from normal queries?
9. Do injection strings remain literal filters, and is Codex rejected rather than substituted?
10. Can two sources safely contain the same native session ID?

## API evidence

Source of truth: installed `/openapi.json` and official V2 docs:

- https://opencode.ai/v2/docs/build/client
- https://opencode.ai/v2/docs/build/plugins
- https://opencode.ai/v2/openapi.json

`/api/event` is live-only and volatile. The experimental session log exposes
exclusive `after` and `follow`, but actual retained replay must be inspected.
Never infer complete historical coverage merely from endpoint availability.
