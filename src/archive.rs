//! Owner-produced, consistent index snapshots.
//!
//! A live WAL index must not be copied file-by-file: the main file, `-wal` and
//! `-shm` change independently. `VACUUM INTO` reads one consistent transaction
//! and writes a compact, self-contained rollback-journal database. The copy is
//! then integrity-checked and hashed before it is published under its final
//! name, so a partially written snapshot never appears complete.
//!
//! Snapshots contain the same private plaintext transcripts as the index. They
//! are created owner-only and must be encrypted before leaving the machine.

use crate::store::Store;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const SNAPSHOT_SCHEMA: &str = "mneme.snapshot.v1";
/// Highest index schema this binary can read and restore.
pub const INDEX_USER_VERSION: i64 = 2;
const REQUIRED_TABLES: &[&str] = &[
    "meta",
    "sources",
    "sessions",
    "messages",
    "message_fts",
    "tool_runs",
    "events",
    "checkpoints",
    "snapshot_hashes",
];
const SENSITIVITY: &str = "Private: plaintext transcripts and tool output, possibly including printed credentials. Keep owner-only; encrypt before it leaves this machine.";

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn create_private(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn partial_name(target: &Path) -> Result<PathBuf> {
    let name = target
        .file_name()
        .context("snapshot path needs a file name")?
        .to_string_lossy();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(target.with_file_name(format!(".{name}.partial-{}-{nonce}", std::process::id())))
}

/// Removes an unpublished temporary file unless explicitly kept.
struct Partial(Option<PathBuf>);
impl Partial {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("partial path present")
    }
}
impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(sidecar(&path, "-journal"));
        }
    }
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    let mut total = 0_u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Write a consistent, verified snapshot of `store` to `out`. Never overwrites.
pub fn create(store: &Store, out: &Path) -> Result<Value> {
    ensure!(
        !exists(out),
        "snapshot target {} already exists; refusing to overwrite",
        out.display()
    );
    ensure!(
        store.path.is_file(),
        "index {} does not exist; run mneme collect first",
        store.path.display()
    );
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(
        parent.is_dir(),
        "snapshot directory {} does not exist",
        parent.display()
    );
    let partial = Partial(Some(partial_name(out)?));
    // VACUUM INTO accepts an existing empty file, which lets the snapshot be
    // owner-only from its first byte instead of being chmod-ed afterwards.
    drop(create_private(partial.path())?);
    {
        let source = Connection::open_with_flags(
            &store.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("open index for snapshot")?;
        source.busy_timeout(Duration::from_secs(30))?;
        let target = partial
            .path()
            .to_str()
            .context("snapshot path must be valid UTF-8")?;
        source
            .execute("VACUUM INTO ?1", [target])
            .context("consistent VACUUM INTO snapshot")?;
    }
    {
        // A snapshot is a single file: no WAL/SHM sidecars may be required.
        let copy = Connection::open(partial.path())?;
        let mode: String = copy.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        ensure!(mode == "delete", "snapshot journal mode is {mode}");
    }
    File::open(partial.path())?.sync_all()?;
    let mut manifest = verify(partial.path(), None)?;
    ensure!(
        !exists(out),
        "snapshot target {} appeared concurrently; refusing to overwrite",
        out.display()
    );
    fs::hard_link(partial.path(), out)
        .with_context(|| format!("publish snapshot {}", out.display()))?;
    sync_parent(out)?;
    drop(partial);
    sync_parent(out)?;
    manifest["created_at_ms"] = json!(crate::now_ms());
    manifest["path"] = json!(out);
    Ok(manifest)
}

/// Validate a snapshot file and describe it. Read-only.
pub fn verify(path: &Path, expected_sha256: Option<&str>) -> Result<Value> {
    if let Some(expected) = expected_sha256 {
        ensure!(
            valid_sha256(expected),
            "expected sha256 must be 64 lowercase hex characters"
        );
    }
    let meta = fs::symlink_metadata(path)
        .with_context(|| format!("inspect snapshot {}", path.display()))?;
    ensure!(
        meta.file_type().is_file(),
        "snapshot must be a regular file"
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        ensure!(
            !exists(&sidecar(path, suffix)),
            "snapshot has a {suffix} sidecar; it is not a self-contained export"
        );
    }
    let mut header = [0_u8; 100];
    File::open(path)?
        .read_exact(&mut header)
        .context("snapshot is too small to be SQLite")?;
    ensure!(
        &header[..16] == b"SQLite format 3\0",
        "snapshot is not an SQLite database"
    );
    // Bytes 18/19 are 2 in WAL mode; a WAL-mode file is not self-contained.
    ensure!(
        header[18] == 1 && header[19] == 1,
        "snapshot is in WAL mode; export with mneme snapshot create"
    );
    let (sha256, bytes) = sha256_file(path)?;
    if let Some(expected) = expected_sha256 {
        ensure!(
            sha256 == expected,
            "snapshot digest mismatch: expected {expected}, found {sha256}"
        );
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.execute_batch("PRAGMA query_only=ON;")?;
    let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        (1..=INDEX_USER_VERSION).contains(&version),
        "snapshot schema version {version} is not supported by this binary"
    );
    let mut problems = Vec::new();
    {
        let mut stmt = c.prepare("PRAGMA integrity_check(20)")?;
        for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let row = row?;
            if row != "ok" {
                problems.push(row);
            }
        }
    }
    if !problems.is_empty() {
        bail!("snapshot integrity check failed: {}", problems.join("; "));
    }
    for table in REQUIRED_TABLES {
        let present: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name=?1 AND type='table'",
            [table],
            |r| r.get(0),
        )?;
        ensure!(present == 1, "snapshot is missing table {table}");
    }
    let generation: i64 = c.query_row(
        "SELECT coalesce((SELECT value FROM meta WHERE key='generation'),0)",
        [],
        |r| r.get(0),
    )?;
    let counts: Value = serde_json::from_str(&c.query_row(
        "SELECT json_object('sessions',(SELECT count(*) FROM sessions WHERE deleted=0),'hidden_sessions',(SELECT count(*) FROM sessions WHERE deleted<>0),'messages',(SELECT count(*) FROM messages),'tool_runs',(SELECT count(*) FROM tool_runs),'raw_events',(SELECT count(*) FROM events),'checkpoints',(SELECT count(*) FROM checkpoints))",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    // The docsize shadow table holds one row per indexed document.
    let fts_rows: i64 =
        c.query_row("SELECT count(*) FROM message_fts_docsize", [], |r| r.get(0))?;
    ensure!(
        Some(fts_rows) == counts["messages"].as_i64(),
        "full-text index row count does not match messages"
    );
    let mut sources = Vec::new();
    {
        // Upstream error text and URLs are deliberately omitted.
        let mut stmt = c.prepare("SELECT id,harness,version FROM sources ORDER BY id")?;
        for row in stmt.query_map([], |r| {
            Ok(json!({"source":r.get::<_,String>(0)?,"harness":r.get::<_,String>(1)?,"version":r.get::<_,Option<String>>(2)?}))
        })? {
            sources.push(row?);
        }
    }
    Ok(json!({
        "schema": SNAPSHOT_SCHEMA,
        "format": "sqlite3",
        "mneme_version": env!("CARGO_PKG_VERSION"),
        "index_user_version": version,
        "generation": generation,
        "bytes": bytes,
        "sha256": sha256,
        "integrity": "ok",
        "counts": counts,
        "sources": sources,
        "sensitivity": SENSITIVITY,
        "coverage": "Same evidence as the source index at one consistent read transaction; not a complete OpenCode archive.",
    }))
}

/// Install a verified snapshot as a new index. Refuses any existing index.
pub fn restore(from: &Path, store: &Store, expected_sha256: Option<&str>) -> Result<Value> {
    for path in [
        store.path.clone(),
        sidecar(&store.path, "-wal"),
        sidecar(&store.path, "-shm"),
    ] {
        ensure!(
            !exists(&path),
            "{} already exists; restore only creates a new index",
            path.display()
        );
    }
    let manifest = verify(from, expected_sha256)?;
    let digest = manifest["sha256"].as_str().unwrap_or_default().to_owned();
    if let Some(parent) = store.path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let partial = Partial(Some(partial_name(&store.path)?));
    {
        let mut input = File::open(from)?;
        let mut output = create_private(partial.path())?;
        std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
    }
    let (copied, _) = sha256_file(partial.path())?;
    ensure!(copied == digest, "restored copy changed while copying");
    ensure!(
        !exists(&store.path),
        "{} appeared concurrently; refusing to overwrite",
        store.path.display()
    );
    fs::hard_link(partial.path(), &store.path)?;
    drop(partial);
    sync_parent(&store.path)?;
    // Re-enter normal operating mode (WAL, schema guards) through the owner path.
    store.init()?;
    let mut result = manifest;
    result["restored_to"] = json!(store.path);
    Ok(result)
}
