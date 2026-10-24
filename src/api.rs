use anyhow::{Context, Result, bail};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Clone)]
pub struct Api {
    client: reqwest::Client,
    base: reqwest::Url,
    password: String,
}

impl Api {
    pub fn new(base: &str, password_file: &Path) -> Result<Self> {
        let base = reqwest::Url::parse(base)?;
        // Session history must not be transmitted to arbitrary remote hosts.
        anyhow::ensure!(
            matches!(
                base.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            ),
            "P0 collector only accepts a loopback OpenCode endpoint"
        );
        anyhow::ensure!(
            base.username().is_empty() && base.password().is_none(),
            "URL credentials are forbidden"
        );
        let credentials: Value = serde_json::from_slice(&std::fs::read(password_file)?)?;
        let password = credentials["password"]
            .as_str()
            .context("service credential file needs password")?
            .to_owned();
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            password,
        })
    }

    fn request(&self, path: &str, query: &[(&str, String)]) -> Result<reqwest::RequestBuilder> {
        Ok(self
            .client
            .get(self.base.join(path)?)
            .basic_auth("opencode", Some(&self.password))
            .query(query))
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let response = self
            .request(path, query)?
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        // Do not include response bodies or credentials in errors.
        anyhow::ensure!(
            response.status().is_success(),
            "OpenCode GET {path}: {}",
            response.status()
        );
        Ok(response.json().await?)
    }

    pub async fn verify(&self) -> Result<Value> {
        let info = self.get("/api/info", &[]).await?;
        let schema = self.get("/openapi.json", &[]).await?;
        for path in [
            "/api/session",
            "/api/session/{sessionID}/message",
            "/api/experimental/session/{sessionID}/log",
            "/api/event",
        ] {
            anyhow::ensure!(
                schema["paths"][path]["get"].is_object(),
                "Required GET endpoint unavailable: {path}"
            );
        }
        Ok(serde_json::json!({"version":info["version"],"compatible":true}))
    }

    pub async fn sessions(&self) -> Result<Vec<Value>> {
        self.pages("/api/session").await
    }

    pub async fn messages(&self, sid: &str) -> Result<Vec<Value>> {
        validate_id(sid)?;
        self.pages(&format!("/api/session/{sid}/message")).await
    }

    async fn pages(&self, path: &str) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        let mut cursor = None::<String>;
        let mut seen = std::collections::HashSet::new();
        loop {
            let mut query = vec![("limit", "100".to_owned())];
            if let Some(c) = cursor.as_ref() {
                query.push(("cursor", c.clone()));
            } else {
                query.push(("order", "asc".to_owned()));
            }
            let page = self.get(path, &query).await?;
            let rows = page["data"]
                .as_array()
                .context("API page missing data array")?;
            if rows.is_empty() {
                break;
            }
            result.extend(rows.iter().cloned());
            cursor = page["cursor"]["next"].as_str().map(str::to_owned);
            let Some(c) = cursor.as_ref() else {
                break;
            };
            anyhow::ensure!(
                seen.insert(c.clone()),
                "API returned a repeated pagination cursor"
            );
        }
        Ok(result)
    }

    pub async fn log(&self, sid: &str, after: Option<i64>) -> Result<Vec<Value>> {
        validate_id(sid)?;
        let mut query = vec![("follow", "false".to_owned())];
        if let Some(seq) = after {
            query.push(("after", seq.to_string()));
        }
        let response = self
            .request(&format!("/api/experimental/session/{sid}/log"), &query)?
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "session.log: {}",
            response.status()
        );
        let mut stream = response.bytes_stream().eventsource();
        let mut rows = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event?;
            if event.event == "effect/http-api/stream/failure" {
                bail!("session.log stream failure");
            }
            if !event.data.is_empty() {
                rows.push(decode(&event.data)?);
            }
        }
        Ok(rows)
    }

    /// Notifications only: persisted sequence advancement happens during reconciliation.
    pub async fn follow(
        &self,
        sid: Option<&str>,
        after: Option<i64>,
        tx: tokio::sync::mpsc::Sender<()>,
    ) -> Result<()> {
        let (path, mut query) = if let Some(sid) = sid {
            validate_id(sid)?;
            (
                format!("/api/experimental/session/{sid}/log"),
                vec![("follow", "true".to_owned())],
            )
        } else {
            ("/api/event".to_owned(), vec![])
        };
        if let Some(seq) = after {
            query.push(("after", seq.to_string()));
        }
        let response = self.request(&path, &query)?.send().await?;
        anyhow::ensure!(
            response.status().is_success(),
            "event stream: {}",
            response.status()
        );
        let mut stream = response.bytes_stream().eventsource();
        while let Some(event) = stream.next().await {
            let event = event?;
            if event.event == "effect/http-api/stream/failure" {
                bail!("event stream overflow/failure");
            }
            if !event.data.is_empty() {
                let value = decode(&event.data)?;
                if !matches!(
                    value["type"].as_str(),
                    Some("log.synced" | "server.connected" | "server.heartbeat")
                ) {
                    // Coalesced wakeups: full reconciliation is the correctness boundary.
                    let _ = tx.try_send(());
                }
            }
        }
        bail!("event stream ended; reconnect required")
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 256
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "Invalid source identifier"
    );
    Ok(())
}

fn decode(text: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(text)?;
    if let Some(s) = value.as_str() {
        Ok(serde_json::from_str(s)?)
    } else {
        Ok(value)
    }
}
