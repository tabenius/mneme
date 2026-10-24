use ragbaz_mneme::store::{Query, Store};
use serde_json::json;

#[test]
fn curated_search_explains_aliases_and_avoids_unsupported_favorites() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    store.init().unwrap();
    store.source("synthetic", "2.test").unwrap();
    store.ingest("synthetic",&json!({"id":"ses_test","time":{"created":1,"updated":2}}),&[
        json!({"id":"msg_repo","type":"user","time":{"created":1},"text":"The repository contains source code."}),
        json!({"id":"msg_directory","type":"user","time":{"created":2},"text":"This directory has photos."}),
        json!({"id":"msg_priority","type":"user","time":{"created":3},"text":"Low prio project: the microphone adapter."})],&[]).unwrap();
    let args = |v| serde_json::from_value::<Query>(v).unwrap();
    let p = store
        .query("history_explain_query", &args(json!({"query":"repo"})))
        .unwrap();
    assert!(
        p["expanded_terms"]
            .as_array()
            .unwrap()
            .contains(&json!("repository"))
    );
    let result = store
        .query(
            "history_search",
            &args(json!({"query":"repo","mode":"concept"})),
        )
        .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["message_id"], "msg_repo");
    let unsupported = store
        .query(
            "history_search",
            &args(json!({"query":"favorite project","mode":"concept"})),
        )
        .unwrap();
    assert_eq!(unsupported["items"], json!([]));
    let priority = store
        .query(
            "history_search",
            &args(json!({"query":"high priority project","mode":"concept"})),
        )
        .unwrap();
    assert_eq!(priority["items"], json!([]));
    assert!(
        store
            .query(
                "history_search",
                &args(json!({"query":"repository","mode":"semantic"}))
            )
            .is_err()
    );
}

#[test]
fn explicit_entity_scope_is_not_widened_by_query_mentions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    store.init().unwrap();
    store.source("fixture", "2.test").unwrap();
    store.ingest("fixture",&json!({"id":"ses_scope","time":{"created":1,"updated":2}}),&[
        json!({"id":"msg_mneme","type":"user","time":{"created":1},"text":"Mneme repository notes"}),
        json!({"id":"msg_frog","type":"user","time":{"created":2},"text":"Frog repository notes"})],&[]).unwrap();
    let q = serde_json::from_value::<Query>(
        json!({"query":"frog repository","entity":"project:mneme","mode":"concept"}),
    )
    .unwrap();
    let result = store.query("history_search", &q).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["message_id"], "msg_mneme");
}
