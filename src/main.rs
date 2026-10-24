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
                    "history_index_status"
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
    }
    Ok(())
}
