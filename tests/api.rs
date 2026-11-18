use ragbaz_mneme::{
    api::Api,
    collect,
    store::{Query, Store},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// A synthetic GET-only OpenCode server: two cursor pages and resumable SSE.
async fn server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("GET "));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: basic ")
                );
                let path = request.split_whitespace().nth(1).unwrap();
                let (content_type, body) = if path == "/api/info" {
                    ("application/json", json!({"version":"2.mock"}).to_string())
                } else if path == "/openapi.json" {
                    ("application/json",json!({"paths":{
                        "/api/session":{"get":{}},"/api/session/{sessionID}/message":{"get":{}},
                        "/api/experimental/session/{sessionID}/log":{"get":{}},"/api/event":{"get":{}}
                    }}).to_string())
                } else if path.starts_with("/api/session?") {
                    if path.contains("cursor=second") {
                        assert!(!path.contains("order="));
                        (
                            "application/json",
                            json!({"data":[],"cursor":{"next":null}}).to_string(),
                        )
                    } else {
                        ("application/json",json!({"data":[{"id":"ses_mock","title":"Synthetic only","time":{"created":1,"updated":2},"location":{"directory":"/fixture"}}],"cursor":{"next":"second"}}).to_string())
                    }
                } else if path.starts_with("/api/session/ses_mock/message?") {
                    if path.contains("cursor=second") {
                        assert!(!path.contains("order="));
                        ("application/json",json!({"data":[{"id":"msg_2","type":"assistant","time":{"created":3},"content":[{"type":"text","text":"Synthetic answer"}]}],"cursor":{"next":null}}).to_string())
                    } else {
                        ("application/json",json!({"data":[{"id":"msg_1","type":"user","time":{"created":1},"text":"Synthetic question"}],"cursor":{"next":"second"}}).to_string())
                    }
                } else if path.starts_with("/api/experimental/session/ses_mock/log?") {
                    if path.contains("after=4") {
                        (
                            "text/event-stream",
                            "data: {\"type\":\"log.synced\",\"seq\":4}\n\n".to_owned(),
                        )
                    } else {
                        ("text/event-stream","data: {\"type\":\"future.event\",\"seq\":4,\"aggregateID\":\"ses_mock\"}\n\ndata: {\"type\":\"log.synced\",\"seq\":4}\n\n".to_owned())
                    }
                } else if path == "/api/event" {
                    ("text/event-stream","data: {\"type\":\"server.connected\"}\n\ndata: {\"type\":\"synthetic.change\"}\n\n".to_owned())
                } else {
                    panic!("Unexpected path {path}")
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
        }
    });
    (url, task)
}

#[tokio::test]
async fn paginated_backfill_sse_resume_and_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let password = dir.path().join("credential.json");
    std::fs::write(&password, r#"{"password":"synthetic"}"#).unwrap();
    let (url, server) = server().await;
    let api = Api::new(&url, &password).unwrap();
    assert_eq!(api.verify().await.unwrap()["compatible"], true);
    let store = Store::new(dir.path().join("index.sqlite"));
    store.init().unwrap();
    store.source("mock", "2.mock").unwrap();
    collect::once(&api, &store, "mock", true, false)
        .await
        .unwrap();
    assert_eq!(store.checkpoint("mock", "ses_mock").unwrap(), Some(4));
    let first = store
        .query("history_index_status", &Query::default())
        .unwrap();
    assert_eq!(first["counts"]["messages"], 2);
    assert_eq!(first["counts"]["raw_events"], 1);
    let generation = store
        .query("history_list_sessions", &Query::default())
        .unwrap()["generation"]
        .clone();
    // Replay after the saved sequence and unchanged snapshot must not churn FTS or invalidate cursors.
    collect::once(&api, &store, "mock", true, false)
        .await
        .unwrap();
    let second = store
        .query("history_list_sessions", &Query::default())
        .unwrap();
    assert_eq!(second["generation"], generation);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    assert!(api.follow(None, None, tx).await.is_err()); // clean EOF is a reconnect boundary
    assert_eq!(rx.recv().await, Some(()));
    let logs = api.log("ses_mock", Some(4)).await.unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["type"], "log.synced");
    let status: Value = store
        .query("history_index_status", &Query::default())
        .unwrap();
    assert_eq!(status["counts"]["raw_events"], 1);
    server.abort();
}

#[test]
fn remote_endpoints_and_identifier_traversal_are_rejected() {
    assert!(Api::new("https://example.com", std::path::Path::new("unused")).is_err());
    assert!(ragbaz_mneme::api::validate_id("../secret").is_err());
}

async fn pages_server(pages: Vec<Value>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for page in pages {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            let body = page.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    (url, task)
}

#[tokio::test]
async fn empty_intermediate_page_does_not_truncate_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let credential = dir.path().join("credential.json");
    std::fs::write(&credential, r#"{"password":"synthetic"}"#).unwrap();
    let (url, server) = pages_server(vec![
        json!({"data":[],"cursor":{"next":"second"}}),
        json!({"data":[{"id":"ses_kept"}],"cursor":{"next":null}}),
    ])
    .await;
    let api = Api::new(&url, &credential).unwrap();
    assert_eq!(
        api.sessions().await.unwrap(),
        vec![json!({"id":"ses_kept"})]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn invalid_cursor_never_hides_previously_indexed_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let credential = dir.path().join("credential.json");
    std::fs::write(&credential, r#"{"password":"synthetic"}"#).unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    store.init().unwrap();
    store.source("mock", "2.mock").unwrap();
    store
        .ingest("mock", &json!({"id":"ses_kept"}), &[], &[])
        .unwrap();
    let (url, server) = pages_server(vec![json!({"data":[],"cursor":{"next":42}})]).await;
    let api = Api::new(&url, &credential).unwrap();
    assert!(
        collect::once(&api, &store, "mock", false, false)
            .await
            .is_err()
    );
    let listing = store
        .query("history_list_sessions", &Query::default())
        .unwrap();
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    server.await.unwrap();
}

#[tokio::test]
async fn collector_retries_initial_outage_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let credential = dir.path().join("credential.json");
    std::fs::write(&credential, r#"{"password":"synthetic"}"#).unwrap();
    // Reserve an endpoint, then release it to simulate an upstream that starts late.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let api = Api::new(&format!("http://{address}"), &credential).unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    let collector = tokio::spawn(collect::run(
        api,
        store.clone(),
        "mock".into(),
        2,
        true,
        None,
    ));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !collector.is_finished(),
        "startup outage must not terminate the collector"
    );
    let listener = TcpListener::bind(address).await.unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut buffer = [0; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8_lossy(&bytes);
                let body = if request.starts_with("GET /api/info ") {
                    json!({"version":"2.mock"})
                } else if request.starts_with("GET /openapi.json ") {
                    json!({"paths":{
                        "/api/session":{"get":{}},"/api/session/{sessionID}/message":{"get":{}},
                        "/api/experimental/session/{sessionID}/log":{"get":{}},"/api/event":{"get":{}}
                    }})
                } else {
                    json!({"data":[],"cursor":{"next":null}})
                }.to_string();
                let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let status = store
                .query("history_index_status", &Query::default())
                .unwrap();
            if status["sources"][0]["connection"] == "connected" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    collector.abort();
    let _ = collector.await;
    server.abort();
    assert!(
        recovered.is_ok(),
        "collector did not recover when upstream became available"
    );
    assert!(
        collect::lock(&store).is_ok(),
        "cancelled collector must release its writer lock"
    );
}

#[tokio::test]
async fn empty_inventory_requires_an_explicit_override() {
    let dir = tempfile::tempdir().unwrap();
    let credential = dir.path().join("credential.json");
    std::fs::write(&credential, r#"{"password":"synthetic"}"#).unwrap();
    let store = Store::new(dir.path().join("index.sqlite"));
    store.init().unwrap();
    store.source("mock", "2.mock").unwrap();
    store
        .ingest("mock", &json!({"id":"ses_kept"}), &[], &[])
        .unwrap();

    let (url, server) = pages_server(vec![json!({"data":[],"cursor":{"next":null}})]).await;
    let api = Api::new(&url, &credential).unwrap();
    let error = collect::once(&api, &store, "mock", false, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("empty session inventory"));
    let listing = store
        .query("history_list_sessions", &Query::default())
        .unwrap();
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    server.await.unwrap();

    let (url, server) = pages_server(vec![json!({"data":[],"cursor":{"next":null}})]).await;
    let api = Api::new(&url, &credential).unwrap();
    collect::once(&api, &store, "mock", false, true)
        .await
        .unwrap();
    let listing = store
        .query("history_list_sessions", &Query::default())
        .unwrap();
    assert_eq!(listing["items"], json!([]));
    server.await.unwrap();
}

#[tokio::test]
async fn pagination_caps_refuse_a_partial_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let credential = dir.path().join("credential.json");
    std::fs::write(&credential, r#"{"password":"synthetic"}"#).unwrap();
    // One page more than the cap.
    let pages: Vec<Value> = (0..=1000)
        .map(|i| json!({"data":[],"cursor":{"next":format!("c{i}")}}))
        .collect();
    let (url, server) = pages_server(pages).await;
    let api = Api::new(&url, &credential).unwrap();
    let error = api.sessions().await.unwrap_err();
    assert!(error.to_string().contains("pagination exceeded"));
    server.abort();

    // A single page with more rows than the cap is also refused.
    let rows: Vec<Value> = (0..=100_000)
        .map(|i| json!({"id":format!("s{i}")}))
        .collect();
    let (url, server) = pages_server(vec![json!({"data":rows,"cursor":{"next":null}})]).await;
    let api = Api::new(&url, &credential).unwrap();
    let error = api.sessions().await.unwrap_err();
    assert!(error.to_string().contains("exceeds"));
    server.await.unwrap();
}
