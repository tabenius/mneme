use ragbaz_mneme::{
    archive,
    store::{Query, Store},
};
use serde_json::{Value, json};

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
    let store = fixture(dir.path());
    let out = dir.path().join("snap.sqlite");
    let manifest = archive::create(&store, &out).unwrap();
    assert_eq!(manifest["schema"], archive::SNAPSHOT_SCHEMA);
    assert_eq!(manifest["integrity"], "ok");
    assert_eq!(manifest["counts"]["messages"], 7);
    assert_eq!(manifest["sources"][0]["source"], "fixture");
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
