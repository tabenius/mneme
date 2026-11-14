//! Physical erasure of sessions the upstream source has removed.
//!
//! Inventory reconciliation only *hides* removed sessions (`deleted=1`). Their
//! text stays in table pages, in FTS5 segments (an external-content delete only
//! appends a tombstone until segments merge), in free pages and in the WAL.
//! Erasure removes the rows with `secure_delete`, merges the full-text index,
//! truncates the WAL and rewrites the file with `VACUUM`, so the hidden text no
//! longer exists anywhere in the index files.
//!
//! It does not reach copies made earlier: snapshots, backups (including
//! rebsync's) and the semantic model must be pruned or rebuilt separately.

use crate::{collect, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};

const COUNTS: &str = "SELECT json_object(
 'sessions',(SELECT count(*) FROM sessions WHERE deleted<>0),
 'messages',(SELECT count(*) FROM messages WHERE session_key IN (SELECT key FROM sessions WHERE deleted<>0)),
 'tool_runs',(SELECT count(*) FROM tool_runs WHERE session_key IN (SELECT key FROM sessions WHERE deleted<>0)),
 'raw_events',(SELECT count(*) FROM events e WHERE EXISTS (SELECT 1 FROM sessions s WHERE s.deleted<>0 AND s.source=e.source AND s.id=e.session_id)),
 'checkpoints',(SELECT count(*) FROM checkpoints k WHERE EXISTS (SELECT 1 FROM sessions s WHERE s.deleted<>0 AND s.source=k.source AND s.id=k.session_id)))";

/// Report (and with `apply`, physically remove) upstream-removed sessions.
/// Applying requires the collector lock, so a running collector is never raced.
pub fn hidden(store: &Store, apply: bool) -> Result<Value> {
    anyhow::ensure!(
        store.path.is_file(),
        "index {} does not exist",
        store.path.display()
    );
    let _lock = if apply {
        Some(collect::lock(store).context("stop the collector before erasing")?)
    } else {
        None
    };
    let mut c = store.writer()?;
    let found: Value = serde_json::from_str(&c.query_row(COUNTS, [], |r| r.get::<_, String>(0))?)?;
    let semantic = store
        .path
        .parent()
        .map(|p| p.join("semantic/manifest.json").exists())
        .unwrap_or(false);
    let mut report = json!({
        "dry_run": !apply,
        "hidden": found,
        "copies_not_reached": "Earlier snapshots and backups (including rebsync's Mneme layer) still contain erased sessions; prune them separately.",
    });
    if semantic {
        report["semantic_model"] = json!(
            "present and built from earlier text; rebuild with mneme semantic-build after erasure"
        );
    }
    if !apply || found["sessions"].as_i64() == Some(0) {
        return Ok(report);
    }
    // Overwrite freed content instead of leaving it in free pages.
    c.pragma_update(None, "secure_delete", "ON")?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TEMP TABLE doomed AS SELECT key,source,id FROM sessions WHERE deleted<>0;
         DELETE FROM events WHERE EXISTS (SELECT 1 FROM doomed d WHERE d.source=events.source AND d.id=events.session_id);
         DELETE FROM checkpoints WHERE EXISTS (SELECT 1 FROM doomed d WHERE d.source=checkpoints.source AND d.id=checkpoints.session_id);
         DELETE FROM snapshot_hashes WHERE session_key IN (SELECT key FROM doomed);
         DELETE FROM tool_runs WHERE session_key IN (SELECT key FROM doomed);
         DELETE FROM messages WHERE session_key IN (SELECT key FROM doomed);
         DELETE FROM sessions WHERE key IN (SELECT key FROM doomed);
         DROP TABLE doomed;
         UPDATE meta SET value=value+1 WHERE key='generation';",
    )?;
    // External-content FTS5 deletes are tombstones; merging drops the old tokens.
    tx.execute(
        "INSERT INTO message_fts(message_fts) VALUES('optimize')",
        [],
    )?;
    tx.commit()?;
    // Old page images can survive in the WAL; fold and truncate it, rewrite the
    // file, then truncate again so VACUUM's own WAL frames are gone too.
    c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    c.execute_batch("VACUUM;")?;
    let (busy, _, _): (i64, i64, i64) =
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let remaining: Value =
        serde_json::from_str(&c.query_row(COUNTS, [], |r| r.get::<_, String>(0))?)?;
    anyhow::ensure!(
        remaining["sessions"].as_i64() == Some(0),
        "hidden sessions remain after erasure"
    );
    report["erased"] = report["hidden"].take();
    report["hidden"] = remaining;
    report["wal_checkpoint_busy"] = json!(busy != 0);
    if busy != 0 {
        report["warning"] = json!(
            "A reader held the WAL open; old pages may persist in it until readers close. Re-run when MCP clients are idle."
        );
    }
    Ok(report)
}
