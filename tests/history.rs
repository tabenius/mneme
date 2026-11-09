use ragbaz_mneme::store::{Query, Store};
use serde_json::{Value, json};

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    store.init().unwrap();
    store.source("fixture", "2.test").unwrap();
    for n in 1..=6 {
        let sid = format!("ses_{n}");
        let session = json!({"id":sid,"projectID":"project","title":format!("Synthetic session {n}"),"location":{"directory":"/fixture/project"},"time":{"created":n*1000,"updated":n*2000},"model":{"id":"test-model"},"agent":"build","cost":0.0,"tokens":{"input":0,"output":0}});
        let messages = vec![
            json!({"id":format!("msg_{n}"),"type":"user","time":{"created":n*1000},"text":format!("worktree checkpoint {n}; historical claim only")}),
            json!({"id":format!("reply_{n}"),"type":"assistant","time":{"created":n*1000+1},"content":[{"type":"text","text":"A recorded reply"},{"type":"tool","id":format!("call_{n}"),"name":"shell","state":{"status":"completed","input":{"command":"git status"},"output":"synthetic output"},"time":{"created":n*1000+2}}]}),
        ];
        store
            .ingest(
                "fixture",
                &session,
                &messages,
                &[json!({"type":"log.synced","seq":n})],
            )
            .unwrap();
    }
    store.health("fixture", None).unwrap();
    (dir, store)
}
fn query(store: &Store, tool: &str, args: Value) -> Value {
    store
        .query(tool, &serde_json::from_value::<Query>(args).unwrap())
        .unwrap()
}

#[test]
fn latest_five_are_opencode_and_correctly_ordered() {
    let (_d, s) = fixture();
    let v = query(&s, "history_list_sessions", json!({"limit":5}));
    assert_eq!(v["items"].as_array().unwrap().len(), 5);
    assert_eq!(v["items"][0]["session_id"], "ses_6");
    assert_eq!(v["items"][4]["session_id"], "ses_2");
    assert_eq!(v["items"][0]["harness"], "opencode");
    assert_eq!(v["has_more"], true);
}
#[test]
fn pagination_has_no_duplicates_and_rejects_filter_changes() {
    let (_d, s) = fixture();
    let first = query(&s, "history_list_sessions", json!({"limit":3}));
    let next = query(
        &s,
        "history_list_sessions",
        json!({"limit":3,"cursor":first["next_cursor"]}),
    );
    assert_eq!(next["items"][0]["session_id"], "ses_3");
    assert_eq!(next["has_more"], false);
    let changed =
        serde_json::from_value::<Query>(json!({"directory":"other","cursor":first["next_cursor"]}))
            .unwrap();
    assert!(s.query("history_list_sessions", &changed).is_err());
}
#[test]
fn search_literal_and_fts_with_filters() {
    let (_d, s) = fixture();
    for mode in ["literal", "fts"] {
        let v = query(
            &s,
            "history_search",
            json!({"query":"worktree","mode":mode,"role":"user","since_ms":3000,"until_ms":5000}),
        );
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert!(
            v["items"][0]["evidence_ref"]
                .as_str()
                .unwrap()
                .starts_with("mneme:")
        );
    }
}
#[test]
fn all_eight_tools_return_bounded_structured_results() {
    let (_d, s) = fixture();
    for (tool, args) in [
        ("history_list_sessions", json!({})),
        ("history_search", json!({"query":"worktree"})),
        ("history_get_session", json!({"session_id":"ses_1"})),
        (
            "history_get_messages",
            json!({"session_id":"ses_1","sort":"created_asc"}),
        ),
        (
            "history_get_activity",
            json!({"since_ms":1000,"until_ms":2000}),
        ),
        (
            "history_get_tool_runs",
            json!({"tool":"shell","status":"completed"}),
        ),
        ("history_aggregate", json!({"group_by":"role"})),
        ("history_index_status", json!({})),
    ] {
        assert!(query(&s, tool, args).is_object(), "{tool}");
    }
}
#[test]
fn tool_state_and_grouping_are_evidence_not_outcome_claims() {
    let (_d, s) = fixture();
    let v = query(&s, "history_get_tool_runs", json!({"query":"git status"}));
    assert_eq!(v["items"].as_array().unwrap().len(), 6);
    assert_eq!(v["items"][0]["status"], "completed");
    let v = query(&s, "history_aggregate", json!({"group_by":"tool"}));
    assert_eq!(v["items"][0]["count"], 6);
    assert_eq!(v["unit"], "tool_runs");
}
#[test]
fn atomic_failure_keeps_checkpoint_and_messages() {
    let (_d, s) = fixture();
    let bad = json!({"id":"ses_1","time":{"created":1000,"updated":9999}});
    assert!(
        s.ingest(
            "fixture",
            &bad,
            &[json!({"type":"user","text":"no ID"})],
            &[json!({"seq":999})]
        )
        .is_err()
    );
    assert_eq!(s.checkpoint("fixture", "ses_1").unwrap(), Some(1));
    assert_eq!(
        query(&s, "history_get_messages", json!({"session_id":"ses_1"}))["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn interrupted_schema_initialization_rolls_back_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    let connection = rusqlite::Connection::open(&store.path).unwrap();
    // Force a failure halfway through schema creation, after meta/sessions.
    connection
        .execute_batch("PRAGMA user_version=1; CREATE TABLE messages(invalid INTEGER);")
        .unwrap();
    assert!(store.init().is_err());
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let partial: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('meta','sessions')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        partial, 0,
        "failed initialization must not leave a partial schema"
    );
    connection.execute_batch("DROP TABLE messages;").unwrap();
    store.init().unwrap();
    assert!(
        store
            .query("history_index_status", &Query::default())
            .is_ok()
    );
}
#[test]
fn replay_is_idempotent_and_unknown_events_are_preserved() {
    let (_d, s) = fixture();
    let session = json!({"id":"ses_1","time":{"created":1000,"updated":2000}});
    let message = json!({"id":"replacement","type":"synthetic","time":{"created":1001},"text":"replacement snapshot"});
    let event = json!({"type":"future.event","seq":9,"unknown":true});
    for _ in 0..2 {
        s.ingest(
            "fixture",
            &session,
            std::slice::from_ref(&message),
            std::slice::from_ref(&event),
        )
        .unwrap();
    }
    assert_eq!(s.checkpoint("fixture", "ses_1").unwrap(), Some(9));
    let v = query(&s, "history_index_status", json!({}));
    assert_eq!(v["counts"]["raw_events"], 1);
    assert_eq!(v["counts"]["messages"], 11);
    let reset = json!({"seq":1,"type":"log.synced"});
    assert!(s.ingest("fixture", &session, &[], &[reset]).is_err());
    assert_eq!(s.checkpoint("fixture", "ses_1").unwrap(), Some(9));
}
#[test]
fn removal_hides_sessions_and_messages() {
    let (_d, s) = fixture();
    s.reconcile_inventory("fixture", &[]).unwrap();
    assert_eq!(
        query(&s, "history_list_sessions", json!({}))["items"],
        json!([])
    );
    assert_eq!(
        query(&s, "history_search", json!({"query":"worktree"}))["items"],
        json!([])
    );
}
#[test]
fn filters_are_parameterized_and_harness_never_silently_changes() {
    let (_d, s) = fixture();
    assert_eq!(
        query(
            &s,
            "history_list_sessions",
            json!({"directory":"' OR 1=1 --"})
        )["items"],
        json!([])
    );
    assert!(
        s.query(
            "history_list_sessions",
            &serde_json::from_value::<Query>(json!({"harness":"codex"})).unwrap()
        )
        .is_err()
    );
    assert!(
        s.query(
            "history_list_sessions",
            &Query {
                limit: Some(1000),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(serde_json::from_value::<Query>(json!({"unknown_field":true})).is_err());
}
#[test]
fn source_identity_prevents_session_id_collisions() {
    let (_d, s) = fixture();
    s.source("second", "2.test").unwrap();
    s.ingest(
        "second",
        &json!({"id":"ses_1","title":"Other source","time":{"created":1,"updated":2}}),
        &[],
        &[],
    )
    .unwrap();
    let v = query(
        &s,
        "history_get_session",
        json!({"source":"second","session_id":"ses_1"}),
    );
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["items"][0]["title"], "Other source");
}

#[test]
fn long_messages_are_retrievable_in_bounded_chunks() {
    let (_d, s) = fixture();
    let text = format!("{}needle", "x".repeat(2500));
    s.ingest(
        "fixture",
        &json!({"id":"ses_long","time":{"created":1,"updated":2}}),
        &[json!({"id":"msg_long","type":"user","time":{"created":1},"text":text})],
        &[],
    )
    .unwrap();
    let first = query(&s, "history_get_messages", json!({"session_id":"ses_long"}));
    assert_eq!(first["items"][0]["truncated"], true);
    let next = query(
        &s,
        "history_get_messages",
        json!({"session_id":"ses_long","message_id":"msg_long","text_offset":2500}),
    );
    assert_eq!(next["items"][0]["text"], "needle");
    assert_eq!(next["items"][0]["truncated"], false);
}

#[test]
fn concurrent_updates_expire_cursors_explicitly() {
    let (_d, s) = fixture();
    let first = query(&s, "history_list_sessions", json!({"limit":1}));
    s.ingest(
        "fixture",
        &json!({"id":"ses_new","time":{"created":9,"updated":10}}),
        &[],
        &[],
    )
    .unwrap();
    assert!(
        s.query(
            "history_list_sessions",
            &serde_json::from_value::<Query>(json!({"cursor":first["next_cursor"]})).unwrap()
        )
        .is_err()
    );
}

#[test]
fn only_one_collector_can_own_an_index() {
    let (_d, s) = fixture();
    let first = ragbaz_mneme::collect::lock(&s).unwrap();
    assert!(ragbaz_mneme::collect::lock(&s).is_err());
    drop(first);
    assert!(ragbaz_mneme::collect::lock(&s).is_ok());
}
