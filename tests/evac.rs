use flate2::read::GzDecoder;
use ragbaz_mneme::evac::{self, Bundle};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

fn fixture(project: &str) -> Value {
    json!({"project":project,"worktree":"/synthetic/not-read","goal":"recover",
        "state":"Claim: tests passed; not independently checked",
        "open_files":["/synthetic/not-read/private"],"decisions":[],"blockers":[],
        "next_action":"Run tests","verify":["cargo test"],"refs":["untrusted:fixture"],
        "last_verified_at":0})
}

fn input(dir: &Path, name: &str, value: &Value) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

#[test]
fn deterministic_bundle_and_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let a = input(dir.path(), "a.json", &fixture("a"));
    let b = input(dir.path(), "b.json", &fixture("b"));
    let one = dir.path().join("one.gz");
    let two = dir.path().join("two.gz");
    let summary = evac::build(&[a.clone(), b.clone()], &one, "evac", 123).unwrap();
    // Formatting and input order do not change bytes.
    fs::write(&a, serde_json::to_string_pretty(&fixture("a")).unwrap()).unwrap();
    evac::build(&[b, a], &two, "evac", 123).unwrap();
    let bytes = fs::read(&one).unwrap();
    assert_eq!(bytes, fs::read(two).unwrap());
    assert_eq!(summary.sha256, evac::sha256(&bytes));
    assert_eq!(summary.size_bytes, bytes.len());
    assert_eq!(&bytes[4..8], &[0, 0, 0, 0]); // No wall-clock gzip timestamp.
    let mut json = Vec::new();
    GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut json)
        .unwrap();
    let mut bundle: Bundle = serde_json::from_slice(&json).unwrap();
    assert_eq!(bundle.schema, "hypomnema.bundle.v1");
    assert_eq!(bundle.items.len(), 2);
    for item in &bundle.items {
        let canonical = evac::canonical_bytes(&bundle.mvhs[&item.project_key]).unwrap();
        assert_eq!(item.mvh_hash, evac::sha256(&canonical));
        assert_eq!(item.size_bytes, canonical.len());
        assert_eq!(item.last_verified_at, 0); // Never invent verification.
    }
    assert_eq!(bundle.mvhs["a"], fixture("a"));
    let id = std::mem::take(&mut bundle.bundle_id);
    let identity = evac::canonical_bytes(&serde_json::to_value(&bundle).unwrap()).unwrap();
    assert_eq!(id, evac::sha256(&identity));
    assert_eq!(id.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(one).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn rejects_bad_schema_and_fields_without_output() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("bundle.gz");
    let mut bad = Vec::new();
    let mut missing = fixture("a");
    missing.as_object_mut().unwrap().remove("verify");
    bad.push(missing);
    let mut unknown = fixture("a");
    unknown["extra"] = json!("no");
    bad.push(unknown);
    let mut wrong_type = fixture("a");
    wrong_type["refs"] = json!("not a list");
    bad.push(wrong_type);
    let mut negative = fixture("a");
    negative["last_verified_at"] = json!(-1);
    bad.push(negative);
    let mut blank = fixture("a");
    blank["state"] = json!("  ");
    bad.push(blank);
    let mut huge = fixture("a");
    huge["goal"] = json!("x".repeat(16 * 1024 + 1));
    bad.push(huge);
    let mut list = fixture("a");
    list["refs"] = json!(vec!["x"; 129]);
    bad.push(list);
    bad.push(fixture("not:allowed"));
    bad.push(fixture(&"x".repeat(65)));
    bad.push(fixture("non-ascii-å"));
    for value in bad {
        let path = input(dir.path(), "bad.json", &value);
        assert!(evac::build(&[path], &output, "evac", 1).is_err());
        assert!(!output.exists());
    }
    let path = input(dir.path(), "duplicate.json", &fixture("a"));
    assert!(evac::build(&[path.clone(), path.clone()], &output, "evac", 1).is_err());
    assert!(evac::build(&[], &output, "evac", 1).is_err());
    assert!(evac::build(&vec![path.clone(); 33], &output, "evac", 1).is_err());
    assert!(evac::build(std::slice::from_ref(&path), &output, "../bad", 1).is_err());
    fs::write(path.clone(), vec![b' '; evac::MAX_INPUT_BYTES + 1]).unwrap();
    assert!(evac::build(&[path], &output, "evac", 1).is_err());
    assert!(!output.exists());
}

#[test]
fn rejects_total_input_budget_and_malformed_json() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("bundle.gz");
    let mut inputs = Vec::new();
    for i in 0..9 {
        let path = input(dir.path(), &format!("{i}.json"), &fixture(&format!("p{i}")));
        let mut bytes = fs::read(&path).unwrap();
        bytes.resize(evac::MAX_INPUT_BYTES, b' ');
        fs::write(&path, bytes).unwrap();
        inputs.push(path);
    }
    assert!(
        evac::build(&inputs, &output, "evac", 1)
            .unwrap_err()
            .to_string()
            .contains("total bytes")
    );
    fs::write(&inputs[0], b"{not json}").unwrap();
    assert!(evac::build(&inputs[..1], &output, "evac", 1).is_err());
    assert!(!output.exists());
}

#[test]
fn never_overwrites_destination_or_follows_output_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let path = input(dir.path(), "a.json", &fixture("a"));
    let output = dir.path().join("bundle.gz");
    fs::write(&output, b"prior user data").unwrap();
    assert!(evac::build(std::slice::from_ref(&path), &output, "evac", 1).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"prior user data");
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&output, &link).unwrap();
        assert!(evac::build(std::slice::from_ref(&path), &link, "evac", 1).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"prior user data");
        assert!(evac::build(&[link], &dir.path().join("unused"), "evac", 1).is_err());
    }
    // Temporary files are removed even when publication fails.
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        if cfg!(unix) { 3 } else { 2 }
    );
}

#[test]
fn cli_is_local_and_does_not_create_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = input(dir.path(), "a.json", &fixture("a"));
    let db = dir.path().join("must-not-exist.sqlite");
    let result = Command::new(env!("CARGO_BIN_EXE_mneme"))
        .arg("--db")
        .arg(&db)
        .args(["evac", "build", "--input"])
        .arg(path)
        .arg("--output")
        .arg(dir.path().join("bundle.gz"))
        .args(["--observed-at", "123"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["item_count"], 1);
    assert!(!String::from_utf8_lossy(&result.stdout).contains("Claim:"));
    assert!(!db.exists());
}

#[test]
fn accepts_dash_project_key_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    for (i, key) in ["org/project".to_string(), "x".repeat(64)]
        .iter()
        .enumerate()
    {
        let path = input(dir.path(), "input.json", &fixture(key));
        let summary = evac::build(&[path], &dir.path().join(format!("{i}.gz")), "x", 0).unwrap();
        assert!(summary.bundle_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(summary.bundle_id.len(), 64);
    }
}

async fn sink(status: &str) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/connector/hypomnema/push",
        listener.local_addr().unwrap()
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\nLocation: http://127.0.0.1:1/stolen\r\n\r\n"
    );
    let task = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&chunk[..n]);
                assert!(bytes.len() <= evac::MAX_REQUEST_BYTES + 4096);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            bytes
        })
        .await
        .unwrap()
    });
    (endpoint, task)
}

#[tokio::test]
async fn explicit_push_matches_sink_and_never_follows_redirects() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let dir = tempfile::tempdir().unwrap();
    let path = input(dir.path(), "a.json", &fixture("org/project"));
    let bundle = dir.path().join("bundle.gz");
    let expected = evac::build(&[path], &bundle, "evac", 123).unwrap();
    let credential = dir.path().join("synthetic-token");
    fs::write(&credential, "synthetic-not-a-real-secret\n").unwrap();
    for status in ["201 Created", "302 Found", "401 Unauthorized", "500 Failed"] {
        let (endpoint, server) = sink(status).await;
        let result = evac::push(&bundle, &credential, &endpoint).await;
        assert_eq!(result.is_ok(), status.starts_with("201"));
        let request = server.await.unwrap();
        let end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
        assert!(headers.starts_with("post /api/connector/hypomnema/push http/1.1"));
        assert!(headers.contains("authorization: bearer synthetic-not-a-real-secret"));
        let body: Value = serde_json::from_slice(&request[end + 4..]).unwrap();
        assert_eq!(body.as_object().unwrap().len(), 6);
        assert_eq!(body["bundle_id"], expected.bundle_id);
        assert_eq!(body["schema"], "hypomnema.bundle.v1");
        assert_eq!(body["items"][0]["project_key"], "org/project");
        assert_eq!(body["observed_at"], 123);
        assert_eq!(
            STANDARD.decode(body["data"].as_str().unwrap()).unwrap(),
            fs::read(&bundle).unwrap()
        );
        if let Err(error) = result {
            assert!(!error.to_string().contains("synthetic-not-a-real-secret"));
            assert!(
                error
                    .to_string()
                    .contains(status.split(' ').next().unwrap())
            );
        }
    }
}

#[tokio::test]
async fn push_rejects_unsafe_destinations_and_oversized_payloads_before_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-credentials");
    let bundle = dir.path().join("bundle.gz");
    for endpoint in [
        "http://dash.ragbaz.cc/api/connector/hypomnema/push",
        "https://evil.example/api/connector/hypomnema/push",
        "http://localhost/api/connector/hypomnema/push",
        "http://user:pass@127.0.0.1/api/connector/hypomnema/push",
        "http://127.0.0.1/other",
        "https://dash.ragbaz.cc/api/connector/hypomnema/push?x=1",
    ] {
        let error = evac::push(&bundle, &missing, endpoint).await.unwrap_err();
        assert!(error.to_string().contains("endpoint"));
    }
    fs::write(&bundle, vec![0; evac::MAX_GZIP_BYTES + 1]).unwrap();
    assert!(
        evac::push(&bundle, &missing, evac::PUSH_ENDPOINT)
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    fs::remove_file(&bundle).unwrap();
    let mut inputs = Vec::new();
    for i in 0..20 {
        let mut value = fixture(&format!("project/{i}"));
        value["worktree"] = json!("w".repeat(16 * 1024));
        value["goal"] = json!("g".repeat(16 * 1024));
        inputs.push(input(dir.path(), &format!("{i}.json"), &value));
    }
    evac::build(&inputs, &bundle, "evac", 1).unwrap();
    assert!(
        evac::push(&bundle, &missing, evac::PUSH_ENDPOINT)
            .await
            .unwrap_err()
            .to_string()
            .contains("push request exceeds")
    );
}

#[test]
fn build_rejects_compressed_budget_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    let mut rng = 123456789u64;
    for i in 0..24 {
        let mut value = fixture(&format!("p{i}"));
        // Deterministic synthetic high-entropy ASCII, no external data/randomness.
        for field in ["worktree", "goal", "state", "next_action"] {
            let text: String = (0..16 * 1024)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    (b'!' + (rng % 90) as u8) as char
                })
                .collect();
            value[field] = json!(text);
        }
        inputs.push(input(dir.path(), &format!("{i}.json"), &value));
    }
    let output = dir.path().join("bundle.gz");
    let error = evac::build(&inputs, &output, "evac", 1).unwrap_err();
    assert!(error.to_string().contains("compressed bytes"));
    assert!(!output.exists());
}

#[tokio::test]
async fn push_rejects_decompression_bombs_and_modified_identity() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("bundle.gz");
    let missing = dir.path().join("no-credentials");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&vec![b' '; 3 * 1024 * 1024 + 1]).unwrap();
    fs::write(&bundle, encoder.finish().unwrap()).unwrap();
    assert!(
        evac::push(&bundle, &missing, evac::PUSH_ENDPOINT)
            .await
            .unwrap_err()
            .to_string()
            .contains("decompressed bundle exceeds")
    );
    fs::remove_file(&bundle).unwrap();
    let path = input(dir.path(), "a.json", &fixture("a"));
    evac::build(&[path], &bundle, "evac", 1).unwrap();
    let bytes = fs::read(&bundle).unwrap();
    let mut json = Vec::new();
    GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut json)
        .unwrap();
    let mut value: Value = serde_json::from_slice(&json).unwrap();
    value["observed_at"] = json!(2);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    fs::write(&bundle, encoder.finish().unwrap()).unwrap();
    assert!(
        evac::push(&bundle, &missing, evac::PUSH_ENDPOINT)
            .await
            .unwrap_err()
            .to_string()
            .contains("bundle ID integrity mismatch")
    );
}

#[test]
fn offline_verify_checks_complete_bundle_and_refuses_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let source = input(dir.path(), "source.json", &fixture("offline"));
    let path = dir.path().join("evac.gz");
    let built = evac::build(&[source], &path, "evac", 17).unwrap();
    let verified = evac::inspect(&path).unwrap();
    assert_eq!(verified.bundle_id, built.bundle_id);
    assert_eq!(verified.sha256, built.sha256);
    assert_eq!(verified.item_count, 1);

    let absent_db = dir.path().join("not-created.sqlite");
    let result = Command::new(env!("CARGO_BIN_EXE_mneme"))
        .arg("--db").arg(&absent_db)
        .args(["evac", "verify", "--bundle"]).arg(&path)
        .output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(!absent_db.exists());
    let reported: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(reported["sha256"], built.sha256);

    let mut decoded = Vec::new();
    GzDecoder::new(fs::read(&path).unwrap().as_slice())
        .read_to_end(&mut decoded).unwrap();
    let mut payload: Value = serde_json::from_slice(&decoded).unwrap();
    payload["mvhs"]["offline"]["state"] = json!("false assertion");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    use std::io::Write;
    encoder.write_all(&serde_json::to_vec(&payload).unwrap()).unwrap();
    fs::write(&path, encoder.finish().unwrap()).unwrap();
    assert!(evac::inspect(&path).is_err());
}
