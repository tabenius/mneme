use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ragbaz_mneme::{
    api::Api,
    collect,
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Store::new(args.db.unwrap_or_else(default_db));
    match args.command {
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
            anyhow::ensure!(
                store.path.exists(),
                "Index missing; run mneme collect first"
            );
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
