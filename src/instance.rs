//! Local MCP settings, audit receipts and profiling sidecars.
use crate::{collect, now_ms, store::Store};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use wait_timeout::ChildExt;

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub nostoi: bool,
    pub profiling: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub nostoi: Option<bool>,
    pub profiling: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reclaim {
    /// Run SQLite VACUUM on the local index. Defaults to a space estimate.
    pub apply: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    /// Unique local evaluation run ID, using letters, digits, hyphens or underscores.
    pub run_id: String,
    pub mode: String,
    pub k: u32,
    /// Pooled true positives at k across the judged queries.
    pub true_positives: u32,
    /// Pooled false positives at k across the judged queries.
    pub false_positives: u32,
    /// Pooled false negatives at k within the same judged universe.
    pub false_negatives: u32,
}

fn sidecar(store: &Store, suffix: &str) -> PathBuf {
    let mut path = store.path.as_os_str().to_owned();
    path.push(suffix);
    path.into()
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub fn settings(store: &Store) -> Result<Settings> {
    match fs::read(sidecar(store, ".settings.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid Mneme settings"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn update(store: &Store, patch: SettingsPatch) -> Result<Value> {
    let path = sidecar(store, ".settings.json");
    fs::create_dir_all(parent(&path))?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar(store, ".settings.lock"))?;
    lock.lock()?;
    let previous = settings(store)?;
    let current = Settings {
        nostoi: patch.nostoi.unwrap_or(previous.nostoi),
        profiling: patch.profiling.unwrap_or(previous.profiling),
    };
    if current.nostoi && !previous.nostoi {
        nostoi(&["--version".as_ref()])?;
        let ledger = sidecar(store, ".nostoi.jsonl");
        create_private_file(&ledger)?;
        if ledger.metadata()?.len() > 0 {
            verify(&ledger)?;
        }
    }
    let mut file = tempfile::NamedTempFile::new_in(parent(&path))?;
    serde_json::to_writer_pretty(&mut file, &current)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    fs::File::open(parent(&sidecar(store, ".settings.json")))?.sync_all()?;
    Ok(json!({"previous": previous, "current": current, "effective": "next request"}))
}

fn create_private_file(path: &Path) -> Result<()> {
    if !path.exists() {
        let file = tempfile::NamedTempFile::new_in(parent(path))?;
        match file.persist_noclobber(path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.into()),
        }
    }
    Ok(())
}

fn nostoi(args: &[&std::ffi::OsStr]) -> Result<()> {
    let mut child = Command::new("nostoi")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Cannot start the installed Nostoi CLI")?;
    if let Some(status) = child.wait_timeout(Duration::from_secs(10))? {
        ensure!(status.success(), "Nostoi rejected the operation ({status})");
    } else {
        let _ = child.kill();
        child.wait()?;
        anyhow::bail!("Nostoi timed out");
    }
    Ok(())
}

fn verify(path: &Path) -> Result<()> {
    nostoi(&[
        "verify".as_ref(),
        "--format".as_ref(),
        "nostoi-v1".as_ref(),
        "--".as_ref(),
        path.as_os_str(),
    ])
}

fn commitment(nonce: &[u8], domain: &[u8], value: &impl Serialize) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(nonce);
    digest.update(domain);
    digest.update(serde_json::to_vec(value)?);
    Ok(format!("{:x}", digest.finalize()))
}

pub fn receipt(
    store: &Store,
    tool: &str,
    input: &impl Serialize,
    output: &Value,
    error: bool,
) -> Result<Value> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("Cannot generate audit nonce: {e}"))?;
    let mut input = serde_json::to_value(input)?;
    if let Some(fields) = input.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
    let body = json!({
        "version": 1,
        "tool": tool,
        "input_sha256": commitment(&nonce, b"mneme/input/v1", &input)?,
        "output_sha256": commitment(&nonce, b"mneme/output/v1", output)?,
        "is_error": error
    });
    let path = sidecar(store, ".nostoi.jsonl");
    // Use an absolute path so a filename starting with '-' cannot become an option.
    let path = fs::canonicalize(path).context("Nostoi ledger is missing or inaccessible")?;
    let body_arg = body.to_string();
    nostoi(&[
        "append".as_ref(),
        path.as_os_str(),
        "--kind".as_ref(),
        "mneme.mcp.exchange".as_ref(),
        "--body".as_ref(),
        body_arg.as_ref(),
    ])?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(json!({"commitments": body, "nonce": nonce}))
}

fn profile_db(store: &Store, write: bool) -> Result<Connection> {
    let path = sidecar(store, ".profile.sqlite");
    if write {
        create_private_file(&path)?;
    }
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(Duration::from_secs(2))?;
    if write {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS search_profile (
                id INTEGER PRIMARY KEY, at_ms INTEGER NOT NULL, mode TEXT NOT NULL,
                elapsed_ms INTEGER NOT NULL, hits INTEGER, failed INTEGER NOT NULL,
                index_bytes INTEGER
            );
            CREATE TABLE IF NOT EXISTS retrieval_evaluation (
                run_id TEXT NOT NULL, mode TEXT NOT NULL, k INTEGER NOT NULL,
                at_ms INTEGER NOT NULL, tp INTEGER NOT NULL, fp INTEGER NOT NULL, fn INTEGER NOT NULL,
                PRIMARY KEY (run_id, mode, k)
            );",
        )?;
    }
    Ok(db)
}

fn known_mode(mode: &str) -> bool {
    matches!(
        mode,
        "literal" | "fts" | "ranked_fts" | "concept" | "semantic" | "hybrid"
    )
}

fn file_bytes(store: &Store) -> Option<u64> {
    let main = fs::metadata(&store.path).ok()?.len();
    let wal = fs::metadata(sidecar(store, "-wal")).map_or(0, |m| m.len());
    Some(main.saturating_add(wal))
}

pub fn profile(
    store: &Store,
    mode: &str,
    elapsed_ms: u64,
    hits: Option<usize>,
    failed: bool,
) -> Result<()> {
    let mode = if known_mode(mode) { mode } else { "invalid" };
    let db = profile_db(store, true)?;
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO search_profile(at_ms,mode,elapsed_ms,hits,failed,index_bytes)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![now_ms(), mode, elapsed_ms, hits, failed, file_bytes(store)],
    )?;
    tx.execute(
        "DELETE FROM search_profile WHERE id <= (SELECT max(id)-10000 FROM search_profile)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn evaluation(store: &Store, entry: Evaluation) -> Result<Value> {
    ensure!(
        settings(store)?.profiling,
        "Enable profiling to save evaluations"
    );
    ensure!(
        !entry.run_id.is_empty()
            && entry.run_id.len() <= 80
            && entry
                .run_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid evaluation run ID"
    );
    ensure!(known_mode(&entry.mode), "Invalid evaluation mode");
    ensure!((1..=100).contains(&entry.k), "k must be between 1 and 100");
    let db = profile_db(store, true)?;
    db.execute(
        "INSERT INTO retrieval_evaluation(run_id,mode,k,at_ms,tp,fp,fn)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            entry.run_id,
            entry.mode,
            entry.k,
            now_ms(),
            entry.true_positives,
            entry.false_positives,
            entry.false_negatives
        ],
    )?;
    let tp = f64::from(entry.true_positives);
    let precision_denominator = tp + f64::from(entry.false_positives);
    let recall_denominator = tp + f64::from(entry.false_negatives);
    Ok(
        json!({"precision": (precision_denominator > 0.0).then(|| tp / precision_denominator),
              "recall": (recall_denominator > 0.0).then(|| tp / recall_denominator),
              "basis": "caller-supplied pooled judgments"}),
    )
}

fn history(store: &Store) -> Result<Value> {
    if !sidecar(store, ".profile.sqlite").exists() {
        return Ok(json!({"search": [], "evaluations": [], "growth": null}));
    }
    let db = profile_db(store, false)?;
    let mut query = db.prepare(
        "WITH ranked AS (
            SELECT *, row_number() OVER (PARTITION BY mode ORDER BY elapsed_ms) AS rank,
                count(*) OVER (PARTITION BY mode) AS n
            FROM search_profile
         )
         SELECT mode,count(*),avg(elapsed_ms),sum(failed),sum(hits=0),
            max(CASE WHEN rank = (95*n+99)/100 THEN elapsed_ms END),avg(hits)
         FROM ranked GROUP BY mode ORDER BY mode",
    )?;
    let search = query
        .query_map([], |r| {
            Ok(json!({
                "mode": r.get::<_,String>(0)?, "calls": r.get::<_,i64>(1)?,
                "mean_ms": r.get::<_,f64>(2)?, "failures": r.get::<_,i64>(3)?,
                "zero_hits": r.get::<_,Option<i64>>(4)?,
                "p95_ms": r.get::<_,i64>(5)?, "mean_hits": r.get::<_,Option<f64>>(6)?
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut query = db.prepare(
        "SELECT run_id,mode,k,at_ms,tp,fp,fn,1.0*tp/(tp+fp),1.0*tp/(tp+fn)
         FROM retrieval_evaluation ORDER BY at_ms DESC,run_id,mode,k LIMIT 25",
    )?;
    let evaluations = query.query_map([], |r| Ok(json!({
        "run_id": r.get::<_,String>(0)?, "mode": r.get::<_,String>(1)?, "k": r.get::<_,u32>(2)?,
        "at_ms": r.get::<_,i64>(3)?, "tp": r.get::<_,u32>(4)?, "fp": r.get::<_,u32>(5)?,
        "fn": r.get::<_,u32>(6)?, "precision": r.get::<_,Option<f64>>(7)?,
        "recall": r.get::<_,Option<f64>>(8)?
    })))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut query = db.prepare(
        "SELECT at_ms,index_bytes FROM search_profile
         WHERE id IN (SELECT max(id) FROM search_profile
             WHERE index_bytes IS NOT NULL GROUP BY at_ms / 86400000)
         ORDER BY at_ms DESC LIMIT 14",
    )?;
    let days = query
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let growth = match (days.first(), days.last()) {
        (Some((end, last)), Some((start, first)))
            if days.len() >= 7 && end - start >= 518_400_000 =>
        {
            let rate = (*last - *first) as f64 * 86_400_000.0 / (*end - *start) as f64;
            json!({"samples": days.len(), "from_ms": start, "to_ms": end,
                "observed_bytes_per_day": rate, "projected_30_day_bytes": (*last as f64 + 30.0 * rate).max(0.0),
                "assumption": "linear continuation of observed index and WAL size; workload and maintenance can change it"})
        }
        _ => Value::Null,
    };
    Ok(json!({"search": search,"evaluations":evaluations,"growth":growth}))
}

fn free_pages(db: &Connection) -> Result<u64> {
    let count: u64 = db.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
    let size: u64 = db.pragma_query_value(None, "page_size", |r| r.get(0))?;
    Ok(count.saturating_mul(size))
}

fn checkpoint(db: &Connection) -> Result<()> {
    let busy: u32 = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    ensure!(
        busy == 0,
        "Active readers prevent WAL truncation; retry when idle"
    );
    Ok(())
}

pub fn reclaim(store: &Store, apply: bool) -> Result<Value> {
    if !apply {
        return Ok(json!({"applied": false, "index_bytes": file_bytes(store),
            "free_page_bytes": free_pages(&store.reader()?)?}));
    }
    let _lock = collect::lock(store).context("Stop the collector before compacting")?;
    let db = store.writer()?;
    ensure!(
        store.query("history_index_status", &Default::default())?["cleanup_pending"] == false,
        "Resolve index health or pending physical erasure before compacting"
    );
    let before = file_bytes(store);
    checkpoint(&db)?;
    if free_pages(&db)? > 0 {
        db.execute_batch("VACUUM")?;
    }
    checkpoint(&db).context("VACUUM completed; final WAL truncation remains pending")?;
    let after = file_bytes(store);
    Ok(
        json!({"applied": true, "before_bytes": before, "after_bytes": after,
        "reclaimed_bytes": before.zip(after).map(|(a,b)| a.saturating_sub(b))}),
    )
}

pub fn describe(store: &Store, admin: bool) -> Result<Value> {
    let current = settings(store)?;
    let ledger = sidecar(store, ".nostoi.jsonl");
    let audit_state = if !ledger.exists() {
        "no_ledger"
    } else if !current.nostoi {
        "disabled_with_ledger"
    } else if ledger.metadata()?.len() == 0 {
        "empty_ledger"
    } else if verify(&ledger).is_ok() {
        "verified_local_chain"
    } else {
        "verification_failed"
    };
    let past = history(store)?;
    let index = store.query("history_index_status", &Default::default())?;
    let storage = reclaim(store, false).unwrap_or(Value::Null);
    let mut actions = Vec::new();
    if index["stale"] == true || index["state"] != "ready" {
        actions.push("Check collector coverage and freshness before evaluating retrieval.");
    }
    if storage["free_page_bytes"].as_u64().unwrap_or_default() >= 16 * 1024 * 1024 {
        actions.push("The local index has at least 16 MiB of unused pages. Preview compaction with history_reclaim_space.");
    }
    if past["evaluations"].as_array().is_some_and(Vec::is_empty) {
        actions.push(
            "Record a labeled retrieval baseline to compare precision and recall across modes.",
        );
    }
    if matches!(audit_state, "verification_failed" | "no_ledger") && current.nostoi {
        actions.push(
            "Inspect the missing or damaged ledger against your last independent checkpoint.",
        );
    }
    Ok(json!({
        "current": { "settings": current, "admin_enabled": admin,
            "index": index, "storage": storage, "audit": audit_state },
        "past": past,
        "future": {
            "storage_projection": past["growth"],
            "suggested_actions": actions,
            "retrieval": [
                "Compare ranked FTS and concept expansion at fixed k on held-out query families.",
                "Evaluate character trigrams for typo recall, then bounded edit-distance reranking for identifiers.",
                "Evaluate POS tagging for phrase and entity extraction per language. Test HMMs only on labeled sequence tasks."
            ]
        }
    }))
}
