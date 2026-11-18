use ragbaz_mneme::{
    now_ms, retain,
    store::{Query, Store},
};
use serde_json::json;
use std::path::Path;

const DAY: i64 = 86_400_000;

fn contains(path: &Path, needle: &str) -> bool {
    std::fs::read(path)
        .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
        .unwrap_or(false)
}

fn store(dir: &Path) -> Store {
    let store = Store::new(dir.join("index.sqlite"));
    store.init().unwrap();
    store.source("fixture", "2.test").unwrap();
    store
}

fn session(store: &Store, sid: &str, updated: i64, text: &str) {
    let session = json!({
        "id": sid,
        "projectID": "p",
        "title": sid,
        "location": {"directory": "/fixture"},
        "time": {"created": updated, "updated": updated},
    });
    let messages = vec![json!({
        "id": format!("{sid}_m"),
        "type": "user",
        "time": {"created": updated},
        "text": text,
    })];
    let events = vec![json!({"type": "message.updated", "seq": 1, "text": text})];
    store
        .ingest("fixture", &session, &messages, &events)
        .unwrap();
}

fn index_bytes(store: &Store) -> u64 {
    let mut total = std::fs::metadata(&store.path).map(|m| m.len()).unwrap_or(0);
    let wal = std::path::PathBuf::from(format!("{}-wal", store.path.display()));
    total += std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0);
    total
}

#[test]
fn age_pruning_removes_old_sessions_reclaims_space_and_sets_a_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let now = now_ms();
    session(&store, "ses_old", now - 30 * DAY, "zebrasecretoldx1");
    session(&store, "ses_mid", now - 10 * DAY, "zebrasecretmidx2");
    session(&store, "ses_new", now - DAY, "newvisibletext");
    let wal = std::path::PathBuf::from(format!("{}-wal", store.path.display()));
    assert!(contains(&store.path, "zebrasecretoldx1") || contains(&wal, "zebrasecretoldx1"));

    let policy = retain::Policy {
        older_than_ms: Some(7 * DAY),
        max_bytes: None,
        source: None,
    };
    let dry = retain::prune(&store, &policy, false, false).unwrap();
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["candidates"]["sessions"], json!(2));
    assert!(contains(&store.path, "zebrasecretoldx1"));

    let applied = retain::prune(&store, &policy, true, false).unwrap();
    assert_eq!(applied["pruned"]["sessions"], json!(2));
    assert_eq!(applied["cleanup_pending"], false);
    assert_eq!(
        store.retention_before("fixture").unwrap(),
        Some(now - 10 * DAY)
    );
    for path in [&store.path, &wal] {
        assert!(
            !contains(path, "zebrasecretoldx1") && !contains(path, "zebrasecretmidx2"),
            "{} still holds pruned text",
            path.display()
        );
    }
    let visible = store
        .query("history_list_sessions", &Query::default())
        .unwrap();
    assert_eq!(visible["items"].as_array().unwrap().len(), 1);
    assert_eq!(visible["items"][0]["session_id"], "ses_new");
    // A second run has nothing to do.
    let again = retain::prune(&store, &policy, true, false).unwrap();
    assert_eq!(again["candidates"]["sessions"], json!(0));
    assert_eq!(again["dry_run"], false);
}

#[test]
fn size_pruning_keeps_the_newest_sessions_and_reports_unmet_targets() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let now = now_ms();
    session(
        &store,
        "ses_old",
        now - 3 * DAY,
        &format!("zebrasecretbigx3 {}", "a".repeat(300_000)),
    );
    session(&store, "ses_new", now - DAY, "newsmalltext");
    let before = index_bytes(&store);
    let policy = retain::Policy {
        older_than_ms: None,
        max_bytes: Some(before.saturating_sub(1)),
        source: None,
    };
    let applied = retain::prune(&store, &policy, true, false).unwrap();
    assert_eq!(applied["pruned"]["sessions"], json!(1));
    assert!(applied["pruned"]["estimated_bytes"].as_u64().unwrap() > 0);
    let after = applied["index_bytes_after"].as_u64().unwrap();
    assert!(after <= before, "index grew from {before} to {after}");
    let visible = store
        .query("history_list_sessions", &Query::default())
        .unwrap();
    assert_eq!(visible["items"].as_array().unwrap().len(), 1);
    assert_eq!(visible["items"][0]["session_id"], "ses_new");
    assert!(!contains(&store.path, "zebrasecretbigx3"));

    // The schema and FTS overhead remain, so a zero target is reported unmet
    // instead of being silently claimed as satisfied.
    let policy = retain::Policy {
        older_than_ms: None,
        max_bytes: Some(0),
        source: None,
    };
    let applied = retain::prune(&store, &policy, true, false).unwrap();
    assert_eq!(applied["pruned"]["sessions"], json!(1));
    assert_eq!(applied["target_met"], false);
    assert_eq!(applied["target_met_after"], false);
    assert!(
        applied["warning"]
            .as_str()
            .unwrap()
            .contains("still exceeds")
    );
}

#[test]
fn watermark_keeps_pruned_history_out_of_later_inventories() {
    let mut inventory = vec![
        json!({"id": "old", "time": {"updated": 500}}),
        json!({"id": "same", "time": {"updated": 1000}}),
        json!({"id": "new", "time": {"updated": 1500}}),
    ];
    retain::filter_inventory(&mut inventory, 1000);
    let ids: Vec<_> = inventory
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["new"]);
}

#[test]
fn clearing_retention_is_reported_and_applied_separately() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    session(&store, "ses_old", now_ms() - 30 * DAY, "oldtext");
    let policy = retain::Policy {
        older_than_ms: Some(DAY),
        max_bytes: None,
        source: None,
    };
    retain::prune(&store, &policy, true, false).unwrap();
    assert!(store.retention_before("fixture").unwrap().is_some());
    let dry = retain::clear_retention(&store, None, false).unwrap();
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["cleared"], 1);
    assert!(store.retention_before("fixture").unwrap().is_some());
    let applied = retain::clear_retention(&store, None, true).unwrap();
    assert_eq!(applied["cleared"], 1);
    assert_eq!(store.retention_before("fixture").unwrap(), None);
}

#[test]
fn watermarks_are_never_lowered_by_a_later_prune() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let now = now_ms();
    session(&store, "ses_recent_old", now - 10 * DAY, "firstprunedtext");
    let policy = retain::Policy {
        older_than_ms: Some(7 * DAY),
        max_bytes: None,
        source: None,
    };
    retain::prune(&store, &policy, true, false).unwrap();
    let watermark = store.retention_before("fixture").unwrap().unwrap();

    // An even older session found later must not pull the watermark back:
    // that would make the collector re-ingest the first pruned session.
    session(&store, "ses_older", now - 20 * DAY, "secondprunedtext");
    retain::prune(&store, &policy, true, false).unwrap();
    assert_eq!(store.retention_before("fixture").unwrap(), Some(watermark));
}

#[test]
fn pruning_discards_the_model_derived_from_pruned_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let now = now_ms();
    session(&store, "ses_old", now - 30 * DAY, "oldtext");
    session(&store, "ses_new", now - DAY, "newtext");
    let semantic = dir.path().join("semantic");
    std::fs::create_dir_all(&semantic).unwrap();
    std::fs::write(semantic.join("lsa-1.npz"), "oldtext").unwrap();
    std::fs::write(
        semantic.join("manifest.json"),
        json!({"managed_arrays": ["lsa-1.npz"]}).to_string(),
    )
    .unwrap();
    let policy = retain::Policy {
        older_than_ms: Some(7 * DAY),
        max_bytes: None,
        source: None,
    };
    let report = retain::prune(&store, &policy, true, false).unwrap();
    assert_eq!(
        report["semantic_model"]["discarded"],
        json!(["lsa-1.npz", "manifest.json"])
    );
    assert!(!semantic.join("manifest.json").exists());
    assert!(!semantic.join("lsa-1.npz").exists());
}
