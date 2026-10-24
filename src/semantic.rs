//! Local semantic/concept retrieval bridge. No remote inference or writable MCP tools.
use crate::store::{Query, Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};
use wait_timeout::ChildExt;

pub fn run(store: &Store, tool: &str, query: &Query) -> Result<Value> {
    let mut knowledge: Value = serde_json::from_str(include_str!("../knowledge/base.json"))?;
    if let Some(path) = std::env::var_os("MNEME_KNOWLEDGE") {
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(bytes.len() <= 1024 * 1024, "Knowledge file exceeds 1 MiB");
        let extra: Value = serde_json::from_slice(&bytes)?;
        for field in ["concepts", "entities", "assertions", "candidate_aliases"] {
            if let Some(rows) = extra[field].as_array() {
                knowledge[field]
                    .as_array_mut()
                    .context("Invalid knowledge collection")?
                    .extend(rows.iter().cloned());
            }
        }
    }
    let payload = json!({"db":store.path,"tool":tool,"query":query,"knowledge":knowledge});
    let python = std::env::var_os("MNEME_PYTHON").unwrap_or_else(|| "python3".into());
    let mut child = Command::new(python)
        .args(["-c", include_str!("../scripts/retrieval.py")])
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("OMP_NUM_THREADS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Cannot start local semantic helper; check MNEME_PYTHON")?;
    child
        .stdin
        .take()
        .context("Missing helper stdin")?
        .write_all(&serde_json::to_vec(&payload)?)?;
    // Drain output concurrently: a full pipe must not deadlock before wait_timeout.
    let stdout = child.stdout.take().context("Missing helper stdout")?;
    let stderr = child.stderr.take().context("Missing helper stderr")?;
    let out = std::thread::spawn(move || {
        use std::io::Read;
        let mut b = Vec::new();
        stdout.take(512 * 1024).read_to_end(&mut b).map(|_| b)
    });
    let err = std::thread::spawn(move || {
        use std::io::Read;
        let mut b = Vec::new();
        stderr.take(16 * 1024).read_to_end(&mut b).map(|_| b)
    });
    if child
        .wait_timeout(std::time::Duration::from_secs(15))?
        .is_none()
    {
        child.kill()?;
        child.wait()?;
        let _ = out.join();
        let _ = err.join();
        anyhow::bail!("Local retrieval exceeded 15 seconds; narrow the query");
    }
    let bytes = out
        .join()
        .map_err(|_| anyhow::anyhow!("Helper reader failed"))??;
    let _ = err.join(); // Never expose raw helper tracebacks or document contents.
    let value: Value = serde_json::from_slice(&bytes).context(
        "Local retrieval failed; check optional Python numerical dependencies/model availability",
    )?;
    anyhow::ensure!(
        value.get("error").is_none(),
        "{}",
        value["error"].as_str().unwrap_or("Local retrieval failed")
    );
    Ok(value)
}
