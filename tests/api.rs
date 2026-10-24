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
    collect::once(&api, &store, "mock", true).await.unwrap();
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
    collect::once(&api, &store, "mock", true).await.unwrap();
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
