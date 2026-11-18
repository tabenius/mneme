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
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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
    sha256_reader(&mut file)
}

fn sha256_reader(file: &mut File) -> Result<(String, u64)> {
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

/// One user-visible schema object, compared exactly against a fresh database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaRow {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

fn schema_rows(c: &Connection) -> Result<Vec<SchemaRow>> {
    // FTS5 owns its shadow tables; their SQL can differ between SQLite builds.
    // Everything else must match the schema this binary creates exactly.
    let mut stmt = c.prepare(
        "SELECT type,name,tbl_name,coalesce(sql,'') FROM sqlite_master
         WHERE name NOT GLOB 'sqlite_*' AND name NOT GLOB 'message_fts_*'
         ORDER BY type,name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SchemaRow {
            kind: r.get(0)?,
            name: r.get(1)?,
            table: r.get(2)?,
            sql: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Build the reference schema in memory. This derives the expected objects from
/// the same definition `init` uses, so it cannot silently drift from it.
fn expected_schema() -> Result<Vec<SchemaRow>> {
    let c = Connection::open_in_memory().context("create in-memory schema reference")?;
    c.execute_batch(crate::store::SCHEMA)
        .context("build in-memory schema reference")?;
    schema_rows(&c)
}

/// Reject a database whose tables, indexes or triggers are not exactly the ones
/// this binary owns. Integrity checks prove the file is readable; this proves it
/// is a Mneme index rather than a foreign or tampered SQLite database.
fn check_schema(c: &Connection) -> Result<()> {
    let expected = expected_schema()?;
    let actual = schema_rows(c)?;
    let key = |row: &SchemaRow| format!("{} {}", row.kind, row.name);
    let expected_names: BTreeSet<String> = expected.iter().map(key).collect();
    let actual_names: BTreeSet<String> = actual.iter().map(key).collect();
    if expected_names != actual_names {
        let missing: Vec<String> = expected_names
            .difference(&actual_names)
            .take(3)
            .cloned()
            .collect();
        let unexpected: Vec<String> = actual_names
            .difference(&expected_names)
            .take(3)
            .cloned()
            .collect();
        bail!(
            "snapshot schema does not match this Mneme version (missing: {}; unexpected: {}); restore with matching tooling",
            if missing.is_empty() {
                "none".into()
            } else {
                missing.join(", ")
            },
            if unexpected.is_empty() {
                "none".into()
            } else {
                unexpected.join(", ")
            }
        );
    }
    for row in &actual {
        let reference = expected
            .iter()
            .find(|candidate| candidate.kind == row.kind && candidate.name == row.name)
            .expect("object names are equal");
        ensure!(
            reference.table == row.table && reference.sql == row.sql,
            "snapshot definition for {} {} does not match this Mneme version; restore with matching tooling",
            row.kind,
            row.name
        );
    }
    Ok(())
}

fn check_foreign_keys(c: &Connection) -> Result<()> {
    let mut stmt = c.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let table: String = table.chars().take(64).collect();
        bail!("snapshot has a foreign-key violation in table {table}");
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
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
        // Refuse to export a schema this binary does not understand, and never
        // let the live index's schema run functions while it is being read.
        source.execute_batch("PRAGMA trusted_schema=OFF;")?;
        let version: i64 = source.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version == INDEX_USER_VERSION,
            "index schema version {version} is not the schema this Mneme version creates ({INDEX_USER_VERSION}); upgrade Mneme or re-collect the index"
        );
        check_schema(&source)?;
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
    let mut file = File::open(path).with_context(|| format!("open snapshot {}", path.display()))?;
    let opened = file
        .metadata()
        .with_context(|| format!("inspect open snapshot {}", path.display()))?;
    ensure!(
        same_file(&meta, &opened),
        "snapshot {} changed while it was being opened",
        path.display()
    );
    let mut header = [0_u8; 100];
    file.read_exact(&mut header)
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
    file.seek(SeekFrom::Start(0))
        .context("rewind snapshot for hashing")?;
    let (sha256, bytes) = sha256_reader(&mut file)?;
    if let Some(expected) = expected_sha256 {
        ensure!(
            sha256 == expected,
            "snapshot digest mismatch: expected {expected}, found {sha256}"
        );
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("open snapshot for verification")?;
    // The snapshot's own schema is untrusted input: never let it run functions
    // or write through this connection. Everything below must only read.
    c.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")?;
    let _ = c.execute_batch("PRAGMA defensive=ON;");
    let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        version == INDEX_USER_VERSION,
        "snapshot schema version {version} is not the schema this Mneme version creates ({INDEX_USER_VERSION}); restore with matching tooling"
    );
    check_schema(&c)?;
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
    check_foreign_keys(&c)?;
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
    // The docsize shadow table holds one row per indexed document. With an
    // external-content FTS table every indexed rowid must map back to a message
    // and every message must be indexed; a missing or stale row means the
    // full-text side of the snapshot is incomplete.
    let fts_rows: i64 =
        c.query_row("SELECT count(*) FROM message_fts_docsize", [], |r| r.get(0))?;
    ensure!(
        Some(fts_rows) == counts["messages"].as_i64(),
        "full-text index row count does not match messages"
    );
    let orphan_fts: i64 = c.query_row(
        "SELECT count(*) FROM message_fts_docsize d
         LEFT JOIN messages m ON m.rowid=d.id WHERE m.rowid IS NULL",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        orphan_fts == 0,
        "full-text index has rows without matching messages"
    );
    let missing_fts: i64 = c.query_row(
        "SELECT count(*) FROM messages m
         LEFT JOIN message_fts_docsize d ON d.id=m.rowid WHERE d.id IS NULL",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        missing_fts == 0,
        "messages are missing from the full-text index"
    );
    let fts_config: i64 = c.query_row(
        "SELECT count(*) FROM message_fts_config WHERE k='version'",
        [],
        |r| r.get(0),
    )?;
    ensure!(fts_config == 1, "full-text index configuration is missing");
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
    // The path must still name the inode we opened, hashed and validated;
    // otherwise a concurrent replacement could have slipped different bytes past
    // the checks above.
    let final_meta = fs::symlink_metadata(path)
        .with_context(|| format!("re-inspect snapshot {}", path.display()))?;
    ensure!(
        same_file(&meta, &final_meta) && same_file(&opened, &final_meta),
        "snapshot {} changed during verification",
        path.display()
    );
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
    // On failure remove the half-initialized index so a retry is not blocked by
    // the create-only rule above.
    if let Err(error) = store
        .init()
        .with_context(|| format!("initialize restored index {}", store.path.display()))
    {
        let _ = fs::remove_file(&store.path);
        let _ = fs::remove_file(sidecar(&store.path, "-wal"));
        let _ = fs::remove_file(sidecar(&store.path, "-shm"));
        let _ = fs::remove_file(sidecar(&store.path, "-journal"));
        return Err(error);
    }
    let mut result = manifest;
    result["restored_to"] = json!(store.path);
    Ok(result)
}
