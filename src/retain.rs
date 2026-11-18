//! Bounded retention ("vacuum") for the local index.
//!
//! Pruning removes whole sessions, oldest first, either by age or until the
//! projected index size fits a target. It is modelled on `journalctl
//! --vacuum-time` / `--vacuum-size`: the bound is a policy, not a hard quota,
//! and whole records are removed rather than partially truncating one.
//!
//! Removing rows alone does not free disk space (and leaves text in free pages
//! and the WAL), so an applied prune runs the same physical cleanup as erasure.
//! It also discards the derived semantic model before deleting rows, because
//! that model was built from the pruned text.
//!
//! A per-source watermark records the newest session removed by retention. The
//! collector skips upstream sessions at or below it, so pruning is not undone
//! by the next backfill. A session that is updated upstream above the watermark
//! is collected again. `mneme prune --clear-retention` removes the watermark.

use crate::{collect, erase, now_ms, store::Store};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Remove sessions last updated before `now - older_than_ms`.
    pub older_than_ms: Option<i64>,
    /// Keep pruning oldest sessions until the projected index size fits.
    pub max_bytes: Option<u64>,
    /// Restrict the policy to one source instance.
    pub source: Option<String>,
}

struct Row {
    key: String,
    source: String,
    id: String,
    updated: i64,
    deleted: bool,
    bytes: u64,
}

struct Selection {
    rows: Vec<Row>,
    hidden_sessions: usize,
    estimated_bytes: u64,
    watermarks: BTreeMap<String, i64>,
}

/// Report (and with `apply`, physically remove) sessions outside the policy.
/// Applying requires the collector lock, so a running collector is never raced.
pub fn prune(store: &Store, policy: &Policy, apply: bool, keep_semantic: bool) -> Result<Value> {
    anyhow::ensure!(
        store.path.is_file(),
        "index {} does not exist",
        store.path.display()
    );
    let _lock = if apply {
        Some(collect::lock(store).context("stop the collector before pruning")?)
    } else {
        None
    };
    prune_locked(store, policy, apply, keep_semantic)
}

/// Caller must hold the collector lock when `apply` is true (used by the
/// collector's own periodic enforcement).
pub fn prune_locked(
    store: &Store,
    policy: &Policy,
    apply: bool,
    keep_semantic: bool,
) -> Result<Value> {
    anyhow::ensure!(
        policy.older_than_ms.is_some() || policy.max_bytes.is_some(),
        "retention needs an age or size bound"
    );
    // A dry run must not open the index read-write (or create side files).
    // It also may scan the whole index, so avoid the interactive query deadline.
    let mut c = if apply {
        store.writer()?
    } else {
        store.maintenance_reader()?
    };
    let pending = erase::pending(&c)?;
    let index_bytes = file_bytes(&store.path);
    let rows = load_rows(&c, policy.source.as_deref())?;
    let selection = select(rows, policy, index_bytes);
    let selected_bytes = selection.estimated_bytes;
    let projected = index_bytes.saturating_sub(selected_bytes);
    let target_met = policy.max_bytes.is_none_or(|max| projected <= max);
    let semantic_dir = store
        .path
        .parent()
        .map(|p| p.join("semantic"))
        .filter(|dir| dir.join("manifest.json").is_file());
    let mut report = json!({
        "dry_run": !apply,
        "policy": {
            "older_than_ms": policy.older_than_ms,
            "max_bytes": policy.max_bytes,
            "source": policy.source,
        },
        "index_bytes": index_bytes,
        "candidates": {
            "sessions": selection.rows.len(),
            "hidden_sessions": selection.hidden_sessions,
            "estimated_bytes": selected_bytes,
        },
        "projected_index_bytes": projected,
        "target_bytes": policy.max_bytes,
        "target_met": target_met,
        "watermarks": selection.watermarks,
        "cleanup_pending": pending,
        "copies_not_reached": "Earlier snapshots and backups still contain pruned sessions; prune them separately.",
    });
    if let Some(first) = selection.rows.iter().map(|row| row.updated).min() {
        report["oldest_selected_updated_ms"] = json!(first);
    }
    if let Some(last) = selection.rows.iter().map(|row| row.updated).max() {
        report["newest_selected_updated_ms"] = json!(last);
    }
    if semantic_dir.is_some() && !selection.rows.is_empty() {
        report["semantic_model"] = json!(if keep_semantic {
            "kept (--keep-semantic): it still holds vocabulary and keys derived from pruned text; rebuild with mneme semantic-build"
        } else {
            "will be discarded: it is derived from pruned text; rebuild with mneme semantic-build"
        });
    }
    if !apply {
        return Ok(report);
    }
    if selection.rows.is_empty() {
        if !pending {
            return Ok(report);
        }
        // Finish an interrupted erasure/prune even though nothing new matched.
        erase::finish_cleanup(&c, &mut report)?;
        report["resumed_cleanup"] = json!(true);
        report["index_bytes_after"] = json!(file_bytes(&store.path));
        return Ok(report);
    }
    // Discard the derived model first: a failure aborts before rows are lost.
    if !keep_semantic && let Some(dir) = &semantic_dir {
        let removed = erase::discard_semantic(dir)?;
        report["semantic_model"] = json!({"discarded": removed, "rebuild": "mneme semantic-build"});
    }
    erase::mark_pending(&c)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TEMP TABLE doomed(key TEXT PRIMARY KEY, source TEXT NOT NULL, id TEXT NOT NULL);",
    )?;
    {
        let mut insert = tx.prepare("INSERT INTO doomed VALUES(?1,?2,?3)")?;
        for row in &selection.rows {
            insert.execute(params![row.key, row.source, row.id])?;
        }
    }
    // Record the watermark in the same transaction as the deletes. Never lower
    // an existing watermark: that would silently re-download pruned history.
    for (source, before) in &selection.watermarks {
        tx.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=max(value,excluded.value)",
            params![format!("retention_before:{source}"), before],
        )?;
    }
    erase::delete_doomed(&tx)?;
    let remaining: i64 = tx.query_row(
        "SELECT count(*) FROM sessions WHERE key IN (SELECT key FROM doomed)",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        remaining == 0,
        "selected sessions remain after prune delete"
    );
    tx.execute_batch("DROP TABLE doomed;")?;
    tx.commit()?;
    report["pruned"] = report["candidates"].take();
    erase::finish_cleanup(&c, &mut report)?;
    let after = file_bytes(&store.path);
    report["index_bytes_after"] = json!(after);
    report["target_met_after"] = json!(policy.max_bytes.is_none_or(|max| after <= max));
    if !target_met {
        report["warning"] = json!(
            "The index still exceeds the size target because the remaining recent sessions do not fit. Age pruning or a larger target is required."
        );
    }
    Ok(report)
}

/// Remove retention watermarks so unchanged older sessions may be collected
/// again. This changes policy metadata only, never session data.
pub fn clear_retention(store: &Store, source: Option<&str>, apply: bool) -> Result<Value> {
    if !store.path.is_file() {
        return Ok(json!({"dry_run": !apply, "cleared": 0}));
    }
    if !apply {
        let c = store.maintenance_reader()?;
        let cleared = count_watermarks(&c, source)?;
        return Ok(json!({"dry_run": true, "cleared": cleared}));
    }
    let c = store.writer()?;
    let cleared = match source {
        Some(source) => c.execute(
            "DELETE FROM meta WHERE key=?1",
            [format!("retention_before:{source}")],
        )?,
        None => c.execute("DELETE FROM meta WHERE key LIKE 'retention_before:%'", [])?,
    };
    Ok(json!({"dry_run": false, "cleared": cleared}))
}

/// Drop inventory entries that retention already removed upstream-side.
pub fn filter_inventory(sessions: &mut Vec<Value>, before: i64) {
    sessions.retain(|session| {
        session
            .pointer("/time/updated")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            > before
    });
}

fn count_watermarks(c: &Connection, source: Option<&str>) -> Result<i64> {
    Ok(match source {
        Some(source) => c.query_row(
            "SELECT count(*) FROM meta WHERE key=?1",
            [format!("retention_before:{source}")],
            |r| r.get(0),
        )?,
        None => c.query_row(
            "SELECT count(*) FROM meta WHERE key LIKE 'retention_before:%'",
            [],
            |r| r.get(0),
        )?,
    })
}

fn load_rows(c: &Connection, source: Option<&str>) -> Result<Vec<Row>> {
    let mut stmt = c.prepare(
        "SELECT s.key,s.source,s.id,s.updated,s.deleted,
          length(cast(s.raw AS blob))
          + coalesce((SELECT sum(coalesce(length(cast(m.text AS blob)),0)+coalesce(length(cast(m.raw AS blob)),0)) FROM messages m WHERE m.session_key=s.key),0)
          + coalesce((SELECT sum(coalesce(length(cast(t.input AS blob)),0)+coalesce(length(cast(t.output AS blob)),0)+coalesce(length(cast(t.raw AS blob)),0)) FROM tool_runs t WHERE t.session_key=s.key),0)
          + coalesce((SELECT sum(coalesce(length(cast(e.raw AS blob)),0)) FROM events e WHERE e.source=s.source AND e.session_id=s.id),0)
         FROM sessions s WHERE (?1 IS NULL OR s.source=?1) ORDER BY s.updated,s.key",
    )?;
    let rows = stmt
        .query_map(params![source], |r| {
            Ok(Row {
                key: r.get(0)?,
                source: r.get(1)?,
                id: r.get(2)?,
                updated: r.get(3)?,
                deleted: r.get::<_, i64>(4)? != 0,
                bytes: r.get::<_, i64>(5)?.max(0) as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn select(rows: Vec<Row>, policy: &Policy, index_bytes: u64) -> Selection {
    let cutoff = policy.older_than_ms.map(|age| now_ms().saturating_sub(age));
    let mut selected = HashSet::new();
    let mut hidden_sessions = 0;
    // Sessions already hidden because upstream removed them are always eligible.
    for row in &rows {
        if row.deleted {
            selected.insert(row.key.clone());
            hidden_sessions += 1;
        }
    }
    let hidden_bytes: u64 = rows
        .iter()
        .filter(|row| row.deleted)
        .map(|row| row.bytes)
        .sum();
    let mut visible: Vec<&Row> = rows.iter().filter(|row| !row.deleted).collect();
    visible.sort_by(|a, b| (a.updated, a.key.as_str()).cmp(&(b.updated, b.key.as_str())));
    let mut projected = index_bytes.saturating_sub(hidden_bytes);
    let mut watermarks = BTreeMap::new();
    let mut start = 0;
    while start < visible.len() {
        let updated = visible[start].updated;
        let mut end = start;
        let mut group_bytes = 0u64;
        while end < visible.len() && visible[end].updated == updated {
            group_bytes = group_bytes.saturating_add(visible[end].bytes);
            end += 1;
        }
        // Group by updated timestamp: a size-based cut must not split a group,
        // otherwise the watermark would skip a session that was not pruned.
        let by_age = cutoff.is_some_and(|cutoff| updated < cutoff);
        let by_size = policy.max_bytes.is_some_and(|max| projected > max);
        if !(by_age || by_size) {
            break;
        }
        for row in &visible[start..end] {
            selected.insert(row.key.clone());
            let watermark = watermarks.entry(row.source.clone()).or_insert(updated);
            *watermark = (*watermark).max(updated);
        }
        projected = projected.saturating_sub(group_bytes);
        start = end;
    }
    let selected_rows: Vec<Row> = rows
        .into_iter()
        .filter(|row| selected.contains(&row.key))
        .collect();
    let estimated_bytes = selected_rows.iter().map(|row| row.bytes).sum();
    Selection {
        rows: selected_rows,
        hidden_sessions,
        estimated_bytes,
        watermarks,
    }
}

/// Accept journalctl-style spans such as `30d`, `1w`, `12h`, `90m` or `45s`.
pub fn parse_age(text: &str) -> std::result::Result<i64, String> {
    let value = text.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split);
    let count: i64 = digits
        .parse()
        .map_err(|_| format!("invalid duration {text:?}; use e.g. 30d, 1w or 12h"))?;
    let factor = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" => 1_000,
        "m" | "min" => 60 * 1_000,
        "h" => 60 * 60 * 1_000,
        "d" => 24 * 60 * 60 * 1_000,
        "w" => 7 * 24 * 60 * 60 * 1_000,
        "mo" | "month" => 30 * 24 * 60 * 60 * 1_000,
        "y" => 365 * 24 * 60 * 60 * 1_000,
        _ => return Err(format!("unknown duration unit in {text:?}")),
    };
    count
        .checked_mul(factor)
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("duration {text:?} is out of range"))
}

/// Accept `1G`, `1GiB`, `500M`, `2T` or a plain byte count (binary units).
pub fn parse_size(text: &str) -> std::result::Result<u64, String> {
    let value = text.trim().to_ascii_lowercase();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split);
    let count: u64 = digits
        .parse()
        .map_err(|_| format!("invalid size {text:?}; use e.g. 1G, 500M or 1048576"))?;
    let factor = match unit.trim() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        "t" | "tb" | "tib" => 1024 * 1024 * 1024 * 1024,
        _ => return Err(format!("unknown size unit in {text:?}")),
    };
    count
        .checked_mul(factor)
        .ok_or_else(|| format!("size {text:?} is out of range"))
}

fn file_bytes(path: &Path) -> u64 {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    [path.to_path_buf(), wal]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|meta| meta.len())
        .sum()
}

/// Reject a policy that cannot do anything (used by the CLI).
pub fn require_bound(policy: &Policy) -> Result<()> {
    if policy.older_than_ms.is_none() && policy.max_bytes.is_none() {
        bail!("retention requires --older-than or --max-size");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_age, parse_size};

    #[test]
    fn parses_journalctl_style_durations_and_sizes() {
        assert_eq!(parse_age("30d"), Ok(30 * 86_400_000));
        assert_eq!(parse_age("1w"), Ok(7 * 86_400_000));
        assert_eq!(parse_age("90m"), Ok(90 * 60_000));
        assert_eq!(parse_age("45"), Ok(45_000));
        assert!(parse_age("0d").is_err());
        assert!(parse_age("3fortnights").is_err());
        assert_eq!(parse_size("1G"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_size("1GiB"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_size("500M"), Ok(500 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Ok(1024));
        assert!(parse_size("many").is_err());
    }
}
