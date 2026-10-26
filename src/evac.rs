//! Explicit-input Hypomnema producer. Handoff text is a claim, not proof.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, GzBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_ITEMS: usize = 32;
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_INPUT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_GZIP_BYTES: usize = 256 * 1024;
pub const MAX_REQUEST_BYTES: usize = 512 * 1024;
const MAX_JSON_BYTES: usize = 3 * 1024 * 1024;
pub const PUSH_ENDPOINT: &str = "https://dash.ragbaz.cc/api/connector/hypomnema/push";
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_LIST_ITEMS: usize = 128;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub project: String,
    pub worktree: String,
    pub goal: String,
    pub state: String,
    pub open_files: Vec<String>,
    pub decisions: Vec<String>,
    pub blockers: Vec<String>,
    pub next_action: String,
    pub verify: Vec<String>,
    pub refs: Vec<String>,
    pub last_verified_at: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Item {
    pub project_key: String,
    pub worktree: String,
    pub goal: String,
    pub mvh_hash: String,
    pub size_bytes: usize,
    pub last_verified_at: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Bundle {
    pub schema: String,
    pub bundle_id: String,
    pub name: String,
    pub observed_at: u64,
    pub items: Vec<Item>,
    pub mvhs: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub schema: &'static str,
    pub bundle_id: String,
    pub item_count: usize,
    pub size_bytes: usize,
    pub sha256: String,
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn valid_project(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.as_bytes()[0].is_ascii_alphanumeric()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
}

fn text(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= MAX_TEXT_BYTES,
        "text must contain 1..{MAX_TEXT_BYTES} UTF-8 bytes and not be blank"
    );
    Ok(())
}

fn validate(handoff: &Handoff) -> Result<()> {
    ensure!(valid_project(&handoff.project), "invalid project key");
    for value in [
        &handoff.worktree,
        &handoff.goal,
        &handoff.state,
        &handoff.next_action,
    ] {
        text(value)?;
    }
    // Paths and references are inert strings: never resolve or read them.
    for list in [
        &handoff.open_files,
        &handoff.decisions,
        &handoff.blockers,
        &handoff.verify,
        &handoff.refs,
    ] {
        ensure!(
            list.len() <= MAX_LIST_ITEMS,
            "too many list entries (maximum {MAX_LIST_ITEMS})"
        );
        for value in list {
            text(value)?;
        }
    }
    Ok(())
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Compact JSON with recursively lexicographically sorted object keys, no newline.
pub fn canonical_bytes(value: &serde_json::Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

pub fn build(inputs: &[PathBuf], output: &Path, name: &str, observed_at: u64) -> Result<Summary> {
    ensure!(
        (1..=MAX_ITEMS).contains(&inputs.len()),
        "expected 1..{MAX_ITEMS} inputs"
    );
    ensure!(valid_key(name), "invalid bundle name");
    let mut mvhs = BTreeMap::new();
    let mut total = 0;
    for path in inputs {
        ensure!(
            std::fs::symlink_metadata(path)?.file_type().is_file(),
            "input must be a regular file, not a symlink"
        );
        let file = File::open(path).context("open handoff input")?;
        ensure!(file.metadata()?.is_file(), "input must be a regular file");
        let mut bytes = Vec::new();
        file.take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_INPUT_BYTES,
            "handoff exceeds {MAX_INPUT_BYTES} bytes"
        );
        total += bytes.len();
        ensure!(
            total <= MAX_TOTAL_INPUT_BYTES,
            "inputs exceed {MAX_TOTAL_INPUT_BYTES} total bytes"
        );
        let handoff: Handoff =
            serde_json::from_slice(&bytes).context("invalid handoff JSON/schema")?;
        validate(&handoff).context("invalid handoff fields")?;
        let key = handoff.project.clone();
        ensure!(!mvhs.contains_key(&key), "duplicate project key");
        mvhs.insert(key, serde_json::to_value(handoff)?);
    }
    let mut items = Vec::new();
    for (key, value) in &mvhs {
        let handoff: Handoff = serde_json::from_value(value.clone())?;
        let bytes = canonical_bytes(value)?;
        items.push(Item {
            project_key: key.clone(),
            worktree: handoff.worktree,
            goal: handoff.goal,
            mvh_hash: sha256(&bytes),
            size_bytes: bytes.len(),
            last_verified_at: handoff.last_verified_at,
        });
    }
    let mut bundle = Bundle {
        schema: "hypomnema.bundle.v1".into(),
        bundle_id: String::new(),
        name: name.into(),
        observed_at,
        items,
        mvhs,
    };
    // ID commits to every payload field; hash the envelope with an empty ID.
    let identity = canonical_bytes(&serde_json::to_value(&bundle)?)?;
    bundle.bundle_id = sha256(&identity);
    let json = canonical_bytes(&serde_json::to_value(&bundle)?)?;
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(Vec::new(), Compression::default());
    encoder.write_all(&json)?;
    let compressed = encoder.finish()?;
    ensure!(
        compressed.len() <= MAX_GZIP_BYTES,
        "bundle exceeds {MAX_GZIP_BYTES} compressed bytes"
    );
    let summary = Summary {
        schema: "hypomnema.bundle.v1",
        bundle_id: bundle.bundle_id,
        item_count: bundle.items.len(),
        size_bytes: compressed.len(),
        sha256: sha256(&compressed),
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // tempfile uses mode 0600 on Unix. Publish only after validation/compression;
    // persist_noclobber rejects existing files and symlinks, without truncation.
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).context("create private bundle temporary file")?;
    temp.write_all(&compressed)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output)
        .context("publish bundle (destination must not exist)")?;
    Ok(summary)
}

fn bounded_file(path: &Path, max: usize) -> Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "expected regular file, not symlink"
    );
    let file = File::open(path).context("open explicit input")?;
    ensure!(file.metadata()?.is_file(), "expected regular file");
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "explicit input exceeds {max} bytes");
    Ok(bytes)
}

/// Only the production endpoint or literal loopback IPs may receive credentials.
fn push_url(endpoint: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(endpoint).context("invalid push endpoint")?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "endpoint must not contain credentials, query, or fragment"
    );
    let loopback = url
        .host_str()
        .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    ensure!(
        endpoint == PUSH_ENDPOINT
            || (loopback
                && matches!(url.scheme(), "http" | "https")
                && url.path() == "/api/connector/hypomnema/push"),
        "push endpoint must be Dash or a literal loopback test endpoint"
    );
    Ok(url)
}

/// Explicit upload, never invoked by build/collect/MCP. No retries or redirects.
pub async fn push(bundle_path: &Path, credential_file: &Path, endpoint: &str) -> Result<Summary> {
    let url = push_url(endpoint)?;
    let compressed = bounded_file(bundle_path, MAX_GZIP_BYTES)?;
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut json)
        .context("decode bundle gzip")?;
    ensure!(
        json.len() <= MAX_JSON_BYTES,
        "decompressed bundle exceeds limit"
    );
    let mut bundle: Bundle = serde_json::from_slice(&json).context("invalid bundle JSON")?;
    ensure!(
        bundle.schema == "hypomnema.bundle.v1",
        "invalid bundle schema"
    );
    ensure!(
        valid_key(&bundle.bundle_id) && (2..=64).contains(&bundle.bundle_id.len()),
        "invalid bundle ID"
    );
    let id = std::mem::take(&mut bundle.bundle_id);
    ensure!(
        id == sha256(&canonical_bytes(&serde_json::to_value(&bundle)?)?),
        "bundle ID integrity mismatch"
    );
    bundle.bundle_id = id;
    ensure!(
        valid_key(&bundle.name)
            && (1..=MAX_ITEMS).contains(&bundle.items.len())
            && bundle.items.len() == bundle.mvhs.len(),
        "invalid bundle metadata"
    );
    let mut seen = std::collections::BTreeSet::new();
    for item in &bundle.items {
        ensure!(
            valid_project(&item.project_key) && seen.insert(&item.project_key),
            "invalid or duplicate project key"
        );
        let value = bundle.mvhs.get(&item.project_key).context("missing MVH")?;
        let handoff: Handoff = serde_json::from_value(value.clone()).context("invalid MVH")?;
        validate(&handoff)?;
        let bytes = canonical_bytes(value)?;
        ensure!(
            handoff.project == item.project_key
                && handoff.worktree == item.worktree
                && handoff.goal == item.goal
                && handoff.last_verified_at == item.last_verified_at
                && item.size_bytes == bytes.len()
                && item.mvh_hash == sha256(&bytes),
            "item integrity mismatch"
        );
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "schema": bundle.schema, "bundle_id": bundle.bundle_id, "name": bundle.name,
        "observed_at": bundle.observed_at, "items": bundle.items,
        "data": STANDARD.encode(&compressed)
    }))?;
    ensure!(
        body.len() <= MAX_REQUEST_BYTES,
        "push request exceeds {MAX_REQUEST_BYTES} bytes"
    );
    // Plain bearer token in an explicitly selected file; never log its contents.
    let secret = bounded_file(credential_file, 4096)?;
    let token = std::str::from_utf8(&secret)
        .context("credential must be UTF-8")?
        .trim();
    ensure!(
        !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic()),
        "invalid bearer credential"
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client
        .post(url)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("push transport failed"))?;
    // Never print remote response bodies (which may echo submitted private text).
    ensure!(
        response.status().is_success(),
        "push rejected (HTTP {})",
        response.status().as_u16()
    );
    Ok(Summary {
        schema: "hypomnema.bundle.v1",
        bundle_id: bundle.bundle_id,
        item_count: bundle.items.len(),
        size_bytes: compressed.len(),
        sha256: sha256(&compressed),
    })
}
