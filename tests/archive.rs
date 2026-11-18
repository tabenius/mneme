use ragbaz_mneme::{
    archive,
    store::{Query, Store},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

fn mutate(path: &Path, sql: &str) {
    let c = Connection::open(path).unwrap();
    c.execute_batch("PRAGMA journal_mode=DELETE;").unwrap();
    c.execute_batch(sql).unwrap();
    drop(c);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !Path::new(&format!("{}{suffix}", path.display())).exists(),
            "mutation left a {suffix} sidecar"
        );
    }
}

fn snapshot(dir: &Path) -> (Store, std::path::PathBuf, Value) {
    let store = fixture(dir);
    let out = dir.join("snap.sqlite");
    let manifest = archive::create(&store, &out).unwrap();
    (store, out, manifest)
}

fn fixture(dir: &std::path::Path) -> Store {
    let store = Store::new(dir.join("index.sqlite"));
    store.init().unwrap();
    store.source("fixture", "2.test").unwrap();
    let session = json!({"id":"ses_1","projectID":"p","title":"Synthetic","location":{"directory":"/fixture"},"time":{"created":1000,"updated":2000},"model":{"id":"m"},"agent":"build"});
    let messages: Vec<Value> = (0..7)
        .map(|n| json!({"id":format!("msg_{n}"),"type":if n % 2 == 0 {"user"} else {"assistant"},"time":{"created":1000 + n},"text":format!("turn {n} worktree")}))
        .collect();
    store
        .ingest(
            "fixture",
            &session,
            &messages,
            &[json!({"type":"log.synced","seq":1})],
        )
        .unwrap();
    store
}

#[test]
fn snapshot_is_consistent_verified_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let (store, out, manifest) = snapshot(dir.path());
    assert_eq!(manifest["schema"], archive::SNAPSHOT_SCHEMA);
    assert_eq!(manifest["integrity"], "ok");
    assert_eq!(manifest["counts"]["messages"], 7);
    assert_eq!(manifest["sources"][0]["source"], "fixture");
    // The owner contract rebsync consumes: stable schema name, integrity verdict,
    // a lowercase digest and a byte count that matches the published file.
    assert_eq!(
        manifest["sha256"].as_str().unwrap().len(),
        64,
        "digest must be full SHA-256"
    );
    assert!(
        manifest["sha256"]
            .as_str()
            .unwrap()
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    assert_eq!(
        manifest["bytes"].as_u64().unwrap(),
        std::fs::metadata(&out).unwrap().len()
    );
    assert!(manifest["generation"].is_i64());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // No partial files remain beside the published snapshot.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".partial-"))
        .collect();
    assert!(leftovers.is_empty());
    let sha = manifest["sha256"].as_str().unwrap();
    let again = archive::verify(&out, Some(sha)).unwrap();
    assert_eq!(again["bytes"], manifest["bytes"]);
    assert!(archive::verify(&out, Some(&"0".repeat(64))).is_err());
    assert!(
        archive::create(&store, &out).is_err(),
        "overwrote a snapshot"
    );
    // Re-exporting an unchanged index is byte-identical, which is what lets
    // rebsync reuse its content-addressed chunks.
    let second = dir.path().join("snap2.sqlite");
    let manifest2 = archive::create(&store, &second).unwrap();
    assert_eq!(manifest2["sha256"], manifest["sha256"]);
    assert_eq!(manifest2["bytes"], manifest["bytes"]);
}

#[test]
fn verify_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out, manifest) = snapshot(dir.path());
    let before = std::fs::read(&out).unwrap();
    let before_meta = std::fs::metadata(&out).unwrap();
    archive::verify(&out, Some(manifest["sha256"].as_str().unwrap())).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), before);
    let after_meta = std::fs::metadata(&out).unwrap();
    assert_eq!(after_meta.len(), before_meta.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(after_meta.mtime(), before_meta.mtime());
        assert_eq!(after_meta.mtime_nsec(), before_meta.mtime_nsec());
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!Path::new(&format!("{}{suffix}", out.display())).exists());
    }
}

#[test]
fn verify_rejects_schema_drift_and_unknown_versions() {
    // An extra object is not a Mneme index even though SQLite can read it.
    let dir = tempfile::tempdir().unwrap();
    let (_, out, _) = snapshot(dir.path());
    let extra = dir.path().join("extra.sqlite");
    std::fs::copy(&out, &extra).unwrap();
    mutate(&extra, "CREATE TABLE injected(x INTEGER);");
    assert!(archive::verify(&extra, None).is_err());

    // A redefined trigger keeps the name but changes the owner's definition.
    let trigger = dir.path().join("trigger.sqlite");
    std::fs::copy(&out, &trigger).unwrap();
    mutate(
        &trigger,
        "DROP TRIGGER messages_ai;
         CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN SELECT 1; END;",
    );
    assert!(archive::verify(&trigger, None).is_err());

    // Exact version enforcement: older schemas were never released.
    let version = dir.path().join("version.sqlite");
    std::fs::copy(&out, &version).unwrap();
    mutate(&version, "PRAGMA user_version=1;");
    assert!(archive::verify(&version, None).is_err());
    let future = dir.path().join("future.sqlite");
    std::fs::copy(&out, &future).unwrap();
    mutate(&future, "PRAGMA user_version=3;");
    assert!(archive::verify(&future, None).is_err());
}

#[test]
fn verify_rejects_foreign_key_and_fts_problems() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out, _) = snapshot(dir.path());

    let fk = dir.path().join("fk.sqlite");
    std::fs::copy(&out, &fk).unwrap();
    mutate(
        &fk,
        "PRAGMA foreign_keys=OFF;
         INSERT INTO messages(key,session_key,id,role,created,text,raw)
         VALUES('orphan','no-such-session','m','user',1,'x','{}');",
    );
    let error = archive::verify(&fk, None).unwrap_err().to_string();
    assert!(error.contains("foreign-key"), "unexpected error: {error}");

    // One real docsize row replaced by an orphan keeps the counts equal but
    // proves the full-text index does not describe the message table.
    let fts = dir.path().join("fts.sqlite");
    std::fs::copy(&out, &fts).unwrap();
    mutate(
        &fts,
        "DELETE FROM message_fts_docsize WHERE id=(SELECT min(id) FROM message_fts_docsize);
         INSERT INTO message_fts_docsize(id,sz) VALUES(999999,x'00');",
    );
    let error = archive::verify(&fts, None).unwrap_err().to_string();
    assert!(
        error.contains("full-text index"),
        "unexpected error: {error}"
    );
}

#[test]
fn verify_rejects_sidecars_and_unowned_sources() {
    let dir = tempfile::tempdir().unwrap();
    let (store, out, _) = snapshot(dir.path());
    let wal = Path::new(&format!("{}-wal", out.display())).to_path_buf();
    std::fs::write(&wal, b"").unwrap();
    assert!(archive::verify(&out, None).is_err());
    std::fs::remove_file(&wal).unwrap();

    // `create` must not export an index this binary does not own.
    mutate(&store.path, "PRAGMA user_version=3;");
    assert!(archive::create(&store, &dir.path().join("drift.sqlite")).is_err());
}

#[test]
fn verify_rejects_corruption_and_non_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    // The live index is in WAL mode, so it is not a self-contained snapshot.
    assert!(archive::verify(&store.path, None).is_err());
    let out = dir.path().join("snap.sqlite");
    archive::create(&store, &out).unwrap();
    let mut bytes = std::fs::read(&out).unwrap();
    let middle = bytes.len() / 2;
    for b in &mut bytes[middle..middle + 512] {
        *b ^= 0xff;
    }
    let bad = dir.path().join("bad.sqlite");
    std::fs::write(&bad, &bytes).unwrap();
    assert!(archive::verify(&bad, None).is_err());
    let text = dir.path().join("text.sqlite");
    std::fs::write(&text, b"not a database at all, just some text padding padding padding padding padding padding padding").unwrap();
    assert!(archive::verify(&text, None).is_err());
}

#[test]
fn restore_creates_a_queryable_index_and_refuses_existing() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let out = dir.path().join("snap.sqlite");
    let manifest = archive::create(&store, &out).unwrap();
    let sha = manifest["sha256"].as_str().unwrap();
    assert!(archive::restore(&out, &store, Some(sha)).is_err());
    let restored = Store::new(dir.path().join("restored/index.sqlite"));
    archive::restore(&out, &restored, Some(sha)).unwrap();
    let status = restored
        .query("history_index_status", &Query::default())
        .unwrap();
    assert_eq!(status["counts"]["messages"], 7);
    let hits = restored
        .query(
            "history_search",
            &serde_json::from_value(json!({"query":"worktree","mode":"fts"})).unwrap(),
        )
        .unwrap();
    assert_eq!(hits["items"].as_array().unwrap().len(), 7);
    assert!(archive::restore(&out, &restored, Some(sha)).is_err());
}

#[test]
fn context_window_surrounds_the_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let q = |v: Value| serde_json::from_value::<Query>(v).unwrap();
    let v = store
        .query(
            "history_get_context",
            &q(json!({"session_id":"ses_1","message_id":"msg_3","window":2})),
        )
        .unwrap();
    let ids = |key: &str| {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["message_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(v["anchor"]["message_id"], "msg_3");
    assert_eq!(ids("before"), ["msg_1", "msg_2"]);
    assert_eq!(ids("after"), ["msg_4", "msg_5"]);
    assert_eq!(v["has_more_before"], true);
    assert_eq!(v["has_more_after"], true);
    assert!(
        v["anchor"]["evidence_ref"]
            .as_str()
            .unwrap()
            .starts_with("mneme:")
    );
    let edge = store
        .query(
            "history_get_context",
            &q(json!({"session_id":"ses_1","message_id":"msg_0","window":3})),
        )
        .unwrap();
    assert!(edge["before"].as_array().unwrap().is_empty());
    assert_eq!(edge["has_more_before"], false);
    for bad in [
        json!({"session_id":"ses_1"}),
        json!({"session_id":"ses_1","message_id":"msg_3","window":11}),
        json!({"session_id":"ses_1","message_id":"msg_3","limit":5}),
        json!({"session_id":"ses_1","message_id":"missing"}),
        json!({"session_id":"ses_1","message_id":"msg_3","window":10,"text_limit":16000}),
    ] {
        assert!(store.query("history_get_context", &q(bad)).is_err());
    }
    assert!(
        store
            .query(
                "history_get_messages",
                &q(json!({"session_id":"ses_1","window":2}))
            )
            .is_err()
    );
}
