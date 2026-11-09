use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ragbaz_mneme::{
    api::Api,
    collect, evac,
    mcp::History,
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
    /// Build a private local TF-IDF/LSA snapshot; no network or model download.
    SemanticBuild {
        #[arg(long, default_value_t = 128)]
        dimensions: u32,
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
        Command::Collect {
            url,
            password_file,
            source,
            follow,
            interval,
        } => {
            anyhow::ensure!(
                !source.is_empty() && source.len() <= 128,
                "source must contain 1..128 bytes"
            );
            let api = Api::new(&url, &password_file)?;
            if follow {
                collect::run(api, store, source, interval).await?;
            } else {
                store.init()?;
                let _lock = collect::lock(&store)?;
                let info = api.verify().await?;
                store.source(&source, info["version"].as_str().unwrap_or("unknown"))?;
                collect::once(&api, &store, &source, true).await?;
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
