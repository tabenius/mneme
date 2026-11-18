use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ragbaz_mneme::{
    api::Api,
    archive, collect, erase, evac,
    mcp::History,
    retain,
    store::{Query, Store, default_db},
};
use rmcp::ServiceExt;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Mneme: local evidence-oriented session history index"
)]
struct Args {
    #[arg(long, env = "MNEME_DB", global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Produce a local evacuation bundle from explicit handoff JSON files only.
    Evac {
        #[command(subcommand)]
        command: EvacCommand,
    },
    /// Verify the installed API and backfill once, or collect continuously with --follow.
    Collect {
        #[arg(long, env = "MNEME_URL")]
        url: String,
        #[arg(long, env = "MNEME_PASSWORD_FILE")]
        password_file: PathBuf,
        #[arg(long, default_value = "local-opencode")]
        source: String,
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 15)]
        interval: u64,
        /// Permit an empty upstream session inventory (hides all indexed sessions).
        #[arg(long)]
        allow_empty_inventory: bool,
        /// Remove sessions older than this after each pass (e.g. 30d, 1w, 12h).
        #[arg(long, env = "MNEME_RETAIN_TIME", value_parser = retain::parse_age)]
        retain_time: Option<i64>,
        /// Keep the index near this size by pruning the oldest sessions (e.g. 1G).
        #[arg(long, env = "MNEME_MAX_SIZE", value_parser = retain::parse_size)]
        max_size: Option<u64>,
    },
    /// Run a read-only stdio MCP server. Never starts an ingestion worker.
    Mcp,
    /// Run one of the MCP queries locally; args is JSON, not SQL.
    Query {
        tool: String,
        #[arg(long, default_value = "{}")]
        args: String,
    },
    Status,
    /// Physically remove sessions the upstream source deleted (hidden today).
    /// Dry-run unless --apply; requires the collector to be stopped.
    Erase {
        /// Erase every upstream-removed (hidden) session.
        #[arg(long, required = true)]
        hidden: bool,
        #[arg(long)]
        apply: bool,
        /// Keep the semantic model even though it derives from erased text.
        #[arg(long)]
        keep_semantic: bool,
    },
    /// Bound index space by removing whole sessions, oldest first. Dry-run
    /// unless --apply; requires the collector to be stopped.
    Prune {
        /// Remove sessions last updated before now minus this span (e.g. 30d, 1w).
        #[arg(long, value_parser = retain::parse_age)]
        older_than: Option<i64>,
        /// Keep pruning the oldest sessions until the index fits this size (e.g. 1G).
        #[arg(long, value_parser = retain::parse_size)]
        max_size: Option<u64>,
        /// Restrict the policy to one source instance.
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        apply: bool,
        /// Keep the semantic model even though it derives from pruned text.
        #[arg(long)]
        keep_semantic: bool,
        /// Remove retention watermarks so older sessions may be collected again.
        #[arg(long)]
        clear_retention: bool,
    },
    /// Consistent owner-produced index export, validation and restore.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Build a private local TF-IDF/LSA snapshot; no network or model download.
    SemanticBuild {
        #[arg(long, default_value_t = 128)]
        dimensions: u32,
    },
}

#[derive(Subcommand)]
enum SnapshotCommand {
    /// Write a verified, self-contained copy of the index (VACUUM INTO). Never overwrites.
    Create {
        #[arg(long)]
        out: PathBuf,
    },
    /// Check integrity, schema and digest of a snapshot file; read-only.
    Verify {
        path: PathBuf,
        /// Expected lowercase hex SHA-256 of the whole file.
        #[arg(long)]
        sha256: Option<String>,
    },
    /// Install a verified snapshot as --db. Refuses an existing index.
    Restore {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        sha256: Option<String>,
    },
}

#[derive(Subcommand)]
enum EvacCommand {
    /// Explicitly upload a reviewed bundle; credentials are never discovered.
    Push {
        #[arg(long)]
        bundle: PathBuf,
        /// File containing a plain bearer token, not JSON.
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, default_value = evac::PUSH_ENDPOINT)]
        endpoint: String,
    },
    /// Validate and bundle handoffs; no session scanning, network, or overwrite.
    Build {
        #[arg(long, required = true, num_args = 1..)]
        input: Vec<PathBuf>,
        #[arg(long)]
        output: PathBuf,
        /// Caller-selected observation time, in Unix seconds (not verification).
        #[arg(long)]
        observed_at: u64,
        #[arg(long, default_value = "evac")]
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Store::new(args.db.unwrap_or_else(default_db));
    match args.command {
        Command::Evac {
            command:
                EvacCommand::Push {
                    bundle,
                    credential_file,
                    endpoint,
                },
        } => {
            let summary = evac::push(&bundle, &credential_file, &endpoint).await?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::Evac {
            command:
                EvacCommand::Build {
                    input,
                    output,
                    observed_at,
                    name,
                },
        } => {
            let summary = evac::build(&input, &output, &name, observed_at)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::Erase {
            hidden: _,
            apply,
            keep_semantic,
        } => {
            println!(
                "{}",
                serde_json::to_string(&erase::hidden(&store, apply, keep_semantic)?)?
            );
        }
        Command::Prune {
            older_than,
            max_size,
            source,
            apply,
            keep_semantic,
            clear_retention,
        } => {
            let policy = retain::Policy {
                older_than_ms: older_than,
                max_bytes: max_size,
                source: source.clone(),
            };
            let value = if clear_retention {
                anyhow::ensure!(
                    older_than.is_none() && max_size.is_none(),
                    "--clear-retention cannot be combined with --older-than or --max-size"
                );
                retain::clear_retention(&store, source.as_deref(), apply)?
            } else {
                retain::require_bound(&policy)?;
                retain::prune(&store, &policy, apply, keep_semantic)?
            };
            println!("{}", serde_json::to_string(&value)?);
        }
        Command::Snapshot { command } => {
            let value = match command {
                SnapshotCommand::Create { out } => archive::create(&store, &out)?,
                SnapshotCommand::Verify { path, sha256 } => {
                    archive::verify(&path, sha256.as_deref())?
                }
                SnapshotCommand::Restore { from, sha256 } => {
                    archive::restore(&from, &store, sha256.as_deref())?
                }
            };
            println!("{}", serde_json::to_string(&value)?);
        }
        Command::Collect {
            url,
            password_file,
            source,
            follow,
            interval,
            allow_empty_inventory,
            retain_time,
            max_size,
        } => {
            anyhow::ensure!(
                !source.is_empty() && source.len() <= 128,
                "source must contain 1..128 bytes"
            );
            let retention = (retain_time.is_some() || max_size.is_some()).then(|| retain::Policy {
                older_than_ms: retain_time,
                max_bytes: max_size,
                source: Some(source.clone()),
            });
            let api = Api::new(&url, &password_file)?;
            if follow {
                collect::run(
                    api,
                    store,
                    source,
                    interval,
                    allow_empty_inventory,
                    retention,
                )
                .await?;
            } else {
                store.init()?;
                let _lock = collect::lock(&store)?;
                let info = api.verify().await?;
                store.source(&source, info["version"].as_str().unwrap_or("unknown"))?;
                collect::once(&api, &store, &source, true, allow_empty_inventory).await?;
                if let Some(policy) = &retention {
                    eprintln!(
                        "{}",
                        serde_json::to_string(&retain::prune_locked(&store, policy, true, false)?)?
                    );
                }
                println!(
                    "{}",
                    store.query("history_index_status", &Query::default())?
                );
            }
        }
        Command::Mcp => {
            // Discovery must work even while the collector is unavailable or
            // creating its first index. Each call opens the index afresh and
            // reports errors without terminating the MCP transport.
            let service = History { store }.serve(rmcp::transport::stdio()).await?;
            service.waiting().await?;
        }
        Command::Query { tool, args } => {
            anyhow::ensure!(
                [
                    "history_list_sessions",
                    "history_search",
                    "history_get_session",
                    "history_get_messages",
                    "history_get_context",
                    "history_get_activity",
                    "history_get_tool_runs",
                    "history_aggregate",
                    "history_index_status",
                    "history_explain_query",
                    "history_get_knowledge"
                ]
                .contains(&tool.as_str()),
                "Unknown history tool"
            );
            let q: Query = serde_json::from_str(&args).context("Invalid query JSON")?;
            println!("{}", store.query(&tool, &q)?);
        }
        Command::Status => println!(
            "{}",
            store.query("history_index_status", &Query::default())?
        ),
        Command::SemanticBuild { dimensions } => {
            anyhow::ensure!((2..=512).contains(&dimensions), "dimensions must be 2..512");
            let python = std::env::var_os("MNEME_PYTHON").unwrap_or_else(|| "python3".into());
            let status = std::process::Command::new(python)
                .args(["-c", include_str!("../scripts/build-semantic.py"), "--db"])
                .arg(&store.path)
                .arg("--dimensions")
                .arg(dimensions.to_string())
                .env("OPENBLAS_NUM_THREADS", "2")
                .env("OMP_NUM_THREADS", "2")
                .status()?;
            anyhow::ensure!(
                status.success(),
                "Local model build failed; check numerical dependencies"
            );
        }
    }
    Ok(())
}
