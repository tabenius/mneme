//! Local semantic/concept retrieval bridge. No remote inference or writable MCP tools.
use crate::store::{Query, Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Child, Command, Stdio},
    time::Duration,
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
    let mut command = Command::new(python);
    command
        .args(["-c", include_str!("../scripts/retrieval.py")])
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("OMP_NUM_THREADS", "1");
    invoke(
        command,
        serde_json::to_vec(&payload)?,
        Duration::from_secs(15),
    )
}

// std::process::Child does not kill or reap on drop, including on early errors.
struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn invoke(mut command: Command, payload: Vec<u8>, timeout: Duration) -> Result<Value> {
    let mut worker = Worker(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Cannot start local semantic helper; check MNEME_PYTHON")?,
    );
    let child = &mut worker.0;
    let mut stdin = child.stdin.take().context("Missing helper stdin")?;
    let stdout = child.stdout.take().context("Missing helper stdout")?;
    let stderr = child.stderr.take().context("Missing helper stderr")?;
    // Input can block too (e.g. a hung interpreter never reading a large
    // knowledge payload). Start every pipe before the bounded wait.
    let input = std::thread::spawn(move || stdin.write_all(&payload));
    let out = std::thread::spawn(move || {
        use std::io::Read;
        let mut b = Vec::new();
        stdout.take(512 * 1024 + 1).read_to_end(&mut b).map(|_| b)
    });
    let err = std::thread::spawn(move || {
        use std::io::Read;
        let mut b = Vec::new();
        stderr.take(16 * 1024).read_to_end(&mut b).map(|_| b)
    });
    let status = child.wait_timeout(timeout)?;
    if status.is_none() {
        let _ = child.kill();
        child.wait()?;
        let _ = input.join();
        let _ = out.join();
        let _ = err.join();
        anyhow::bail!("Local retrieval timed out; narrow the query");
    }
    let written = input
        .join()
        .map_err(|_| anyhow::anyhow!("Helper writer failed"))?;
    let bytes = out
        .join()
        .map_err(|_| anyhow::anyhow!("Helper reader failed"))??;
    let _ = err.join(); // Never expose raw helper tracebacks or document contents.
    anyhow::ensure!(
        status.is_some_and(|status| status.success()),
        "Local retrieval helper exited unsuccessfully"
    );
    written.context("Cannot send query to local retrieval helper")?;
    anyhow::ensure!(
        bytes.len() <= 512 * 1024,
        "Local retrieval output exceeds 512 KiB"
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn python(script: &str) -> Command {
        let mut command = Command::new("python3");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn hung_helper_not_reading_input_is_killed() {
        let start = std::time::Instant::now();
        let error = invoke(
            python("import time; time.sleep(60)"),
            vec![b'x'; 1024 * 1024],
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn failed_helper_cannot_return_a_success_payload() {
        let error = invoke(
            python("import sys; sys.stdin.read(); print('{}'); sys.exit(1)"),
            b"{}".to_vec(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exited unsuccessfully"));
    }

    #[test]
    fn oversized_helper_output_is_rejected() {
        assert!(
            invoke(
                python("import sys; sys.stdin.read(); print(' ' * (513 * 1024) + '{}')"),
                b"{}".to_vec(),
                Duration::from_secs(5),
            )
            .is_err()
        );
    }
}
