# Hypomnema (evacuation bundle)

Hypomnema is a pre-staged, continuously-improving evacuation bundle for cold-start recovery. It's designed for transfer to LAN, cloud, or USB in under a minute with minimal size.

## Bundle format (v1)
```json
{
  "schema": "hypomnema.bundle.v1",
  "bundle_id": "evac-YYYYMMDD-HHMMSS",
  "name": "evac",
  "observed_at": 1234567890,
  "items": [
    {
      "project_key": "mneme",
      "worktree": "/srv/ragbaz/src/vendors/ragbaz/head/mneme",
      "goal": "ship semantic retrieval",
      "mvh_hash": "sha256...",
      "size_bytes": 2048,
      "last_verified_at": 1234567890
    }
  ],
  "mvhs": { "mneme": { ... } }
}
```
The bundle is gzipped and uploaded via `POST /api/connector/hypomnema/push` with a Rebekah push credential. Metadata is stored in D1, blobs in R2.

## MVH schema
See `schemas/mvh.schema.json` (to be added). Required: project, worktree, goal, state, open_files, decisions, blockers, next_action, verify, refs, last_verified_at.

## CLI (planned)
`mneme evac build` scans worktrees, pulls evidence from Mneme, writes bundle to `~/Handoffs/evac/`. `mneme evac push` uploads to Dash if configured.
