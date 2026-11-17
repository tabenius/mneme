//! Physical erasure of sessions the upstream source has removed.
//!
//! Inventory reconciliation only *hides* removed sessions (`deleted=1`). Their
//! text stays in table pages, in FTS5 segments (an external-content delete only
//! appends a tombstone until segments merge), in free pages and in the WAL.
//! Erasure removes the rows with `secure_delete`, merges the full-text index,
//! truncates the WAL and rewrites the file with `VACUUM`, so the hidden text no
//! longer exists anywhere in the index files.
//!
//! Interruption safety: the rows are deleted in one transaction, but the WAL
//! fold and `VACUUM` that remove the old page images happen afterwards. An
//! `erase_pending` meta marker is committed before the delete, so a crash
//! between the two phases is resumed on the next run even though no hidden
//! session row remains. The marker is cleared only after the final checkpoint
//! is no longer busy.
//!
//! The local semantic model is derived from message text (its manifest holds
//! the vocabulary and per-message keys), so erasure discards the files that
//! model declares it manages, under the builder's own lock. It is discarded
//! *before* any index rows so a failure aborts before erasure begins. It is
//! rebuildable with `mneme semantic-build`. Copies made earlier (snapshots and
//! backups, including rebsync's) are not reachable from here and must be
//! pruned.

use crate::{collect, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{io::Read, path::Path};

/// Set before rows are deleted and cleared only after WAL/VACUUM cleanup.
const PENDING: &str = "erase_pending";
/// Bound a corrupted or hostile semantic manifest rather than reading forever.
const MANIFEST_LIMIT: u64 = 16 * 1024 * 1024;

const COUNTS: &str = "SELECT json_object(
 'sessions',(SELECT count(*) FROM sessions WHERE deleted<>0),
 'messages',(SELECT count(*) FROM messages WHERE session_key IN (SELECT key FROM sessions WHERE deleted<>0)),
 'tool_runs',(SELECT count(*) FROM tool_runs WHERE session_key IN (SELECT key FROM sessions WHERE deleted<>0)),
 'raw_events',(SELECT count(*) FROM events e WHERE EXISTS (SELECT 1 FROM sessions s WHERE s.deleted<>0 AND s.source=e.source AND s.id=e.session_id)),
 'checkpoints',(SELECT count(*) FROM checkpoints k WHERE EXISTS (SELECT 1 FROM sessions s WHERE s.deleted<>0 AND s.source=k.source AND s.id=k.session_id)))";

/// Report (and with `apply`, physically remove) upstream-removed sessions.
/// Applying requires the collector lock, so a running collector is never raced.
pub fn hidden(store: &Store, apply: bool, keep_semantic: bool) -> Result<Value> {
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
    // A dry run must not open the index read-write (or create side files), so
    // it uses the query-only connection.
    let mut c = if apply {
        store.writer()?
    } else {
        store.reader()?
    };
    let found = counts(&c)?;
    let pending = pending(&c)?;
    let hidden_sessions = found["sessions"].as_i64().unwrap_or(0);
    let semantic_dir = store
        .path
        .parent()
        .map(|p| p.join("semantic"))
        .filter(|dir| dir.join("manifest.json").is_file());
    let mut report = json!({
        "dry_run": !apply,
        "hidden": found,
        "cleanup_pending": pending,
        "copies_not_reached": "Earlier snapshots and backups (including rebsync's Mneme layer) still contain erased sessions; prune them separately (rebsync mneme prune).",
    });
    if semantic_dir.is_some() {
        report["semantic_model"] = json!(if keep_semantic {
            "kept (--keep-semantic): it still holds vocabulary and keys derived from erased text; rebuild with mneme semantic-build"
        } else {
            "will be discarded: it is derived from erased text; rebuild with mneme semantic-build"
        });
    }
    if !apply {
        return Ok(report);
    }
    if hidden_sessions == 0 && !pending {
        return Ok(report);
    }
    // Discard the derived model first: if that fails (locked build, unsafe
    // manifest, permission error) the index rows are untouched and a retry is
    // straightforward.
    if hidden_sessions > 0
        && let (Some(dir), false) = (&semantic_dir, keep_semantic)
    {
        let removed = discard_semantic(dir)?;
        report["semantic_model"] = json!({"discarded": removed, "rebuild": "mneme semantic-build"});
    }
    // Overwrite freed content instead of leaving it in free pages.
    c.pragma_update(None, "secure_delete", "ON")?;
    if hidden_sessions > 0 {
        // Persist the intent before deleting: a crash after this commit but
        // before the WAL/VACUUM cleanup is resumed by the next invocation.
        c.execute(
            "INSERT INTO meta(key,value) VALUES(?1,1) ON CONFLICT(key) DO UPDATE SET value=1",
            [PENDING],
        )?;
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
        report["erased"] = report["hidden"].take();
    } else {
        report["resumed_cleanup"] = json!(true);
    }
    // Old page images can survive in the WAL; fold and truncate it, rewrite the
    // file, then truncate again so VACUUM's own WAL frames are gone too. Any
    // error leaves the marker in place so the cleanup can be retried.
    let mut busy = checkpoint(&c)?;
    c.execute_batch("VACUUM;")?;
    busy |= checkpoint(&c)?;
    let remaining = counts(&c)?;
    anyhow::ensure!(
        remaining["sessions"].as_i64() == Some(0),
        "hidden sessions remain after erasure"
    );
    if busy == 0 {
        c.execute("DELETE FROM meta WHERE key=?1", [PENDING])?;
        report["cleanup_pending"] = json!(false);
    } else {
        report["cleanup_pending"] = json!(true);
        report["warning"] = json!(
            "A reader held the WAL open; old pages may persist in it and cleanup is still pending. Re-run when MCP clients are idle."
        );
    }
    report["hidden"] = remaining;
    report["wal_checkpoint_busy"] = json!(busy != 0);
    Ok(report)
}

fn counts(c: &rusqlite::Connection) -> Result<Value> {
    Ok(serde_json::from_str(
        &c.query_row(COUNTS, [], |r| r.get::<_, String>(0))?,
    )?)
}

fn pending(c: &rusqlite::Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT coalesce((SELECT value FROM meta WHERE key=?1),0)<>0",
        [PENDING],
        |r| r.get(0),
    )?)
}

/// Fold and truncate the WAL, returning the number of busy readers (0 is clean).
fn checkpoint(c: &rusqlite::Connection) -> Result<i64> {
    let (busy, _, _): (i64, i64, i64) =
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    Ok(busy)
}

/// Remove the files the semantic manifest declares it manages, then the
/// manifest itself, while holding the builder's lock. Nothing else in the
/// directory is touched. Unsafe names or failed removals abort with the
/// manifest intact, so the cleanup can be retried instead of silently
/// leaving derived text behind.
fn discard_semantic(dir: &Path) -> Result<Vec<String>> {
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .append(true)
        .open(dir.join("build.lock"))?;
    lock.try_lock_exclusive()
        .context("a semantic model build is running; erase again when it finishes")?;
    let manifest: Value = serde_json::from_slice(&read_manifest(&dir.join("manifest.json"))?)
        .context("semantic manifest is invalid; erase aborted before removing model files")?;
    let names = managed_names(&manifest)?;
    let mut removed = Vec::new();
    for name in names {
        match std::fs::remove_file(dir.join(&name)) {
            Ok(()) => removed.push(name),
            // A previous interrupted attempt may already have removed it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot remove semantic file {name}; manifest retained for retry")
                });
            }
        }
    }
    std::fs::remove_file(dir.join("manifest.json"))?;
    removed.push("manifest.json".into());
    let _ = FileExt::unlock(&lock);
    Ok(removed)
}

/// Validate every declared name before removing anything. Only plain file
/// names inside the model directory are accepted; traversal or absolute names
/// are an error rather than a silently skipped entry.
fn managed_names(manifest: &Value) -> Result<Vec<String>> {
    let names: Vec<String> =
        if let Some(arrays) = manifest.get("managed_arrays").and_then(Value::as_array) {
            arrays
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("semantic manifest managed_arrays contains a non-string")
                })
                .collect::<Result<_>>()?
        } else if let Some(array) = manifest.get("arrays").and_then(Value::as_str) {
            vec![array.to_owned()]
        } else {
            anyhow::bail!("semantic manifest declares no managed files; erase aborted");
        };
    for name in &names {
        let plain = !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        anyhow::ensure!(
            plain,
            "semantic manifest declares an unsafe managed file name {name:?}; erase aborted"
        );
    }
    Ok(names)
}

fn read_manifest(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_LIMIT + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MANIFEST_LIMIT,
        "semantic manifest exceeds 16 MiB; erase aborted"
    );
    Ok(bytes)
}
