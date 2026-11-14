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
    let dry = erase::hidden(&store, false).unwrap();
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["hidden"]["sessions"], 1);
    assert_eq!(dry["hidden"]["messages"], 2);
    assert_eq!(dry["hidden"]["tool_runs"], 1);
    assert_eq!(dry["hidden"]["raw_events"], 1);
    assert!(contains(&store.path, SECRET) || contains(&wal, SECRET));

    let before = store
        .query("history_index_status", &Query::default())
        .unwrap();
    let applied = erase::hidden(&store, true).unwrap();
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
        erase::hidden(&store, true).unwrap()["hidden"]["sessions"],
        0
    );
}

#[test]
fn erasure_refuses_while_a_collector_holds_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let _collector = collect::lock(&store).unwrap();
    assert!(erase::hidden(&store, true).is_err());
    // Dry-run never needs the lock and changes nothing.
    assert_eq!(
        erase::hidden(&store, false).unwrap()["hidden"]["sessions"],
        1
    );
}
