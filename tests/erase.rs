use ragbaz_mneme::{
    collect, erase,
    store::{Query, Store},
};
use serde_json::json;

// Lowercase alphanumeric, so the FTS token bytes equal the text bytes.
const SECRET: &str = "zebrasecretx9";

fn contains(path: &std::path::Path, needle: &str) -> bool {
    std::fs::read(path)
        .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
        .unwrap_or(false)
}

fn fixture(dir: &std::path::Path) -> Store {
    let store = Store::new(dir.join("index.sqlite"));
    store.init().unwrap();
    store.source("fixture", "2.test").unwrap();
    for (sid, text) in [("ses_keep", "ordinary worktree note"), ("ses_gone", SECRET)] {
        let session = json!({"id":sid,"projectID":"p","title":sid,"location":{"directory":"/fixture"},"time":{"created":1,"updated":2}});
        let messages = vec![
            json!({"id":format!("{sid}_u"),"type":"user","time":{"created":1},"text":format!("{text} {text}")}),
            json!({"id":format!("{sid}_a"),"type":"assistant","time":{"created":2},"content":[{"type":"tool","id":format!("{sid}_call"),"name":"shell","state":{"status":"completed","input":{"command":text},"output":text},"time":{"created":3}}]}),
        ];
        store
            .ingest(
                "fixture",
                &session,
                &messages,
                &[json!({"type":"message.updated","seq":1,"text":text})],
            )
            .unwrap();
    }
    // Upstream removed ses_gone: reconciliation only hides it.
    store
        .reconcile_inventory("fixture", &[json!({"id":"ses_keep"})])
        .unwrap();
    store
}

#[test]
fn erasure_removes_hidden_text_from_every_index_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let wal = dir.path().join("index.sqlite-wal");
    assert!(
        contains(&store.path, SECRET) || contains(&wal, SECRET),
        "fixture should start with the hidden text on disk"
    );
    let dry = erase::hidden(&store, false, false).unwrap();
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["hidden"]["sessions"], 1);
    assert_eq!(dry["hidden"]["messages"], 2);
    assert_eq!(dry["hidden"]["tool_runs"], 1);
    assert_eq!(dry["hidden"]["raw_events"], 1);
    assert!(contains(&store.path, SECRET) || contains(&wal, SECRET));

    let before = store
        .query("history_index_status", &Query::default())
        .unwrap();
    let applied = erase::hidden(&store, true, false).unwrap();
    assert_eq!(applied["erased"]["sessions"], 1);
    assert_eq!(applied["hidden"]["sessions"], 0);
    assert_eq!(applied["wal_checkpoint_busy"], false);
    for path in [&store.path, &wal] {
        assert!(
            !contains(path, SECRET),
            "{} still holds erased text",
            path.display()
        );
    }
    // Visible history is intact and still searchable through FTS.
    let hits = store
        .query(
            "history_search",
            &serde_json::from_value(json!({"query":"worktree","mode":"fts"})).unwrap(),
        )
        .unwrap();
    assert_eq!(hits["items"].as_array().unwrap().len(), 1);
    let after = store
        .query("history_index_status", &Query::default())
        .unwrap();
    assert_eq!(before["counts"]["sessions"], after["counts"]["sessions"]);
    assert_eq!(after["counts"]["raw_events"], 1);
    // Idempotent.
    assert_eq!(
        erase::hidden(&store, true, false).unwrap()["hidden"]["sessions"],
        0
    );
}

#[test]
fn erasure_refuses_while_a_collector_holds_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let _collector = collect::lock(&store).unwrap();
    assert!(erase::hidden(&store, true, false).is_err());
    // Dry-run never needs the lock and changes nothing.
    assert_eq!(
        erase::hidden(&store, false, false).unwrap()["hidden"]["sessions"],
        1
    );
}

fn semantic_model(dir: &std::path::Path) {
    let folder = dir.join("semantic");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("lsa-1.npz"), format!("vectors {SECRET}")).unwrap();
    std::fs::write(folder.join("unrelated.txt"), "not ours").unwrap();
    let manifest = json!({"method": "lsa", "vocabulary": {SECRET: 0, "worktree": 1},
        "keys": ["k1"], "managed_arrays": ["lsa-1.npz", "../escape"]});
    std::fs::write(folder.join("manifest.json"), manifest.to_string()).unwrap();
}

#[test]
fn erasure_discards_the_semantic_model_derived_from_erased_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    semantic_model(dir.path());
    let dry = erase::hidden(&store, false, false).unwrap();
    assert!(
        dry["semantic_model"]
            .as_str()
            .unwrap()
            .contains("will be discarded")
    );
    assert!(dir.path().join("semantic/manifest.json").exists());
    let applied = erase::hidden(&store, true, false).unwrap();
    assert_eq!(
        applied["semantic_model"]["discarded"],
        json!(["lsa-1.npz", "manifest.json"])
    );
    let folder = dir.path().join("semantic");
    assert!(!folder.join("manifest.json").exists() && !folder.join("lsa-1.npz").exists());
    // Only files the model declared are removed; traversal names are ignored.
    assert!(folder.join("unrelated.txt").exists());
    for entry in std::fs::read_dir(&folder).unwrap().flatten() {
        assert!(
            !contains(&entry.path(), SECRET),
            "{:?} holds erased text",
            entry.path()
        );
    }
    // Status keeps working without a model.
    assert!(
        store
            .query("history_index_status", &Query::default())
            .unwrap()["semantic_model"]
            .is_null()
    );
}

#[test]
fn keep_semantic_is_explicit_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    semantic_model(dir.path());
    let applied = erase::hidden(&store, true, true).unwrap();
    assert!(applied["semantic_model"].as_str().unwrap().contains("kept"));
    assert!(dir.path().join("semantic/manifest.json").exists());
}
