use crate::{api::Api, store::Store};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde_json::Value;
use std::{collections::HashMap, fs::OpenOptions, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

pub fn lock(store: &Store) -> Result<std::fs::File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.path.with_extension("lockfile"))?;
    lock.try_lock_exclusive()
        .context("Another Mneme collector owns this index")?;
    Ok(lock)
}

pub async fn once(api: &Api, store: &Store, source: &str, force: bool) -> Result<Vec<Value>> {
    let sessions = api.sessions().await?;
    let mut recent: Vec<&Value> = sessions.iter().collect();
    recent.sort_by_key(|s| {
        std::cmp::Reverse(
            s.pointer("/time/updated")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        )
    });
    let recent: Vec<&str> = recent
        .into_iter()
        .take(8)
        .filter_map(|s| s["id"].as_str())
        .collect();
    let mut failures = 0;
    for session in &sessions {
        let sid = session["id"].as_str().context("session missing id")?;
        if !(force && recent.contains(&sid)) && !store.needs_sync(source, session)? {
            continue;
        }
        let result = async {
            let events = api.log(sid, store.checkpoint(source, sid)?).await?;
            let messages = api.messages(sid).await?;
            store.ingest(source, session, &messages, &events)
        }
        .await;
        if let Err(error) = result {
            failures += 1;
            // No transcript contents or HTTP bodies are included by Api errors.
            eprintln!("session reconciliation failed ({sid}): {error}");
        }
    }
    if failures > 0 {
        store.health(
            source,
            Some(
                "Some sessions failed reconciliation; retrying without advancing their checkpoints",
            ),
        )?;
        anyhow::bail!("{failures} sessions failed reconciliation");
    }
    store.reconcile_inventory(source, &sessions)?;
    store.health(source, None)?;
    Ok(sessions)
}

pub async fn run(api: Api, store: Store, source: String, interval_secs: u64) -> Result<()> {
    anyhow::ensure!(
        (2..=3600).contains(&interval_secs),
        "interval must be 2..3600 seconds"
    );
    store.init()?;
    let _lock = lock(&store)?;
    let info = api.verify().await?;
    store.source(&source, info["version"].as_str().unwrap_or("unknown"))?;
    let (tx, mut rx) = mpsc::channel(1);
    let global = spawn_follow(api.clone(), store.clone(), source.clone(), None, tx.clone());
    let mut followers = HashMap::<String, JoinHandle<()>>::new();
    let mut last_force = std::time::Instant::now() - Duration::from_secs(61);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let force = last_force.elapsed() >= Duration::from_secs(60);
        let result = tokio::select! {
            _=&mut shutdown=>break,
            result=once(&api,&store,&source,force)=>result,
        };
        match result {
            Ok(mut sessions) => {
                if force {
                    last_force = std::time::Instant::now();
                }
                sessions.sort_by_key(|s| {
                    std::cmp::Reverse(
                        s.pointer("/time/updated")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    )
                });
                let desired: Vec<String> = sessions
                    .iter()
                    .take(8)
                    .filter_map(|s| s["id"].as_str().map(str::to_owned))
                    .collect();
                followers.retain(|sid, handle| {
                    if desired.contains(sid) {
                        true
                    } else {
                        handle.abort();
                        false
                    }
                });
                for sid in desired {
                    followers.entry(sid.clone()).or_insert_with(|| {
                        spawn_follow(
                            api.clone(),
                            store.clone(),
                            source.clone(),
                            Some(sid),
                            tx.clone(),
                        )
                    });
                }
            }
            Err(error) => {
                store.health(
                    &source,
                    Some("Collector reconciliation failed; data may be stale"),
                )?;
                eprintln!("collector: {error}");
            }
        }
        tokio::select! {
            _=&mut shutdown=>break,
            _=tokio::time::sleep(Duration::from_secs(interval_secs))=>{},
            _=rx.recv()=>{
                // Debounce high-rate token/tool notifications.
                tokio::select! {
                    _=&mut shutdown=>break,
                    _=tokio::time::sleep(Duration::from_secs(2))=>{},
                }
                while rx.try_recv().is_ok() {}
            }
        }
    }
    global.abort();
    for (_, handle) in followers {
        handle.abort();
    }
    store.health(
        &source,
        Some("Collector stopped; queryable snapshots retained"),
    )?;
    Ok(())
}

fn spawn_follow(
    api: Api,
    store: Store,
    source: String,
    sid: Option<String>,
    tx: mpsc::Sender<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut delay = 1;
        loop {
            let after = sid
                .as_ref()
                .and_then(|s| store.checkpoint(&source, s).ok().flatten());
            let started = std::time::Instant::now();
            if let Err(error) = api.follow(sid.as_deref(), after, tx.clone()).await {
                eprintln!("event subscription disconnected: {error}; reconnecting");
            }
            let _ = tx.try_send(()); // Reconcile missed notifications after failure.
            if started.elapsed() > Duration::from_secs(60) {
                delay = 1;
            }
            tokio::time::sleep(Duration::from_secs(delay)).await;
            delay = (delay * 2).min(30);
        }
    })
}
