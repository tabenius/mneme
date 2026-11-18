use ragbaz_mneme::{
    instance::{self, Evaluation, SettingsPatch},
    store::Store,
};
use tempfile::tempdir;

#[test]
fn settings_and_profiling_are_opt_in_and_local() {
    let directory = tempdir().unwrap();
    let store = Store {
        path: directory.path().join("index.sqlite"),
    };
    store.init().unwrap();
    assert!(!instance::settings(&store).unwrap().profiling);
    let initial = instance::describe(&store, false).unwrap();
    assert_eq!(initial["past"]["search"], serde_json::json!([]));
    assert!(
        !directory
            .path()
            .join("index.sqlite.profile.sqlite")
            .exists()
    );

    instance::update(
        &store,
        SettingsPatch {
            nostoi: None,
            profiling: Some(true),
        },
    )
    .unwrap();
    instance::profile(&store, "fts", 12, Some(0), false).unwrap();
    let scores = instance::evaluation(
        &store,
        Evaluation {
            run_id: "synthetic-v1".into(),
            mode: "fts".into(),
            k: 5,
            true_positives: 2,
            false_positives: 2,
            false_negatives: 3,
        },
    )
    .unwrap();
    assert_eq!(scores["precision"], 0.5);
    assert_eq!(scores["recall"], 0.4);
    instance::update(
        &store,
        SettingsPatch {
            nostoi: None,
            profiling: Some(false),
        },
    )
    .unwrap();
    let status = instance::describe(&store, true).unwrap();
    assert_eq!(status["past"]["search"][0]["zero_hits"], 1);
    assert_eq!(status["past"]["evaluations"][0]["recall"], 0.4);
    assert_eq!(status["current"]["settings"]["profiling"], false);
    assert!(status["future"]["storage_projection"].is_null());
}

#[test]
fn compaction_reclaims_pages_without_deleting_rows() {
    let directory = tempdir().unwrap();
    let store = Store {
        path: directory.path().join("index.sqlite"),
    };
    store.init().unwrap();
    {
        let db = rusqlite::Connection::open(&store.path).unwrap();
        db.execute_batch(
            "CREATE TABLE fixture (data BLOB);
             INSERT INTO fixture VALUES (zeroblob(262144)), (x'42');
             DELETE FROM fixture WHERE length(data) > 1;",
        )
        .unwrap();
    }
    assert!(
        instance::reclaim(&store, false).unwrap()["free_page_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    let lock = ragbaz_mneme::collect::lock(&store).unwrap();
    assert!(instance::reclaim(&store, true).is_err());
    drop(lock);
    assert!(
        instance::reclaim(&store, true).unwrap()["reclaimed_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    let db = rusqlite::Connection::open(&store.path).unwrap();
    let data: Vec<u8> = db
        .query_row("SELECT data FROM fixture", [], |row| row.get(0))
        .unwrap();
    assert_eq!(data, [0x42]);
}
