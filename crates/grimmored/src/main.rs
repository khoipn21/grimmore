use std::{path::PathBuf, sync::Arc, thread};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use grimmore_core::{
    credentials::RootSecret,
    endpoint::default_endpoint_path,
    protocol::{PROTOCOL_VERSION, WireContract},
};
use grimmored::{
    ipc::{IpcServerConfig, serve},
    mcp::{McpStdioConfig, serve_stdio},
    storage::Storage,
    vault_index::{index_vault, search_notes},
    vault_watcher::VaultWatcher,
};
use rusqlite::Connection;
use schemars::schema_for;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "grimmored", version, about = "Grimmore local companion")]
struct Cli {
    /// Override the per-user operational database path.
    #[arg(long, global = true)]
    database: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect local prerequisites without changing the vault.
    Doctor,
    /// Reconcile one explicitly selected vault into the local FTS index.
    Index {
        #[arg(long)]
        vault_id: String,
        #[arg(long)]
        vault: PathBuf,
    },
    /// Print the canonical JSON Schema used to generate plugin types.
    ProtocolSchema,
    /// Search one indexed vault without reading or changing Markdown.
    Search {
        #[arg(long)]
        vault_id: String,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u16,
    },
    /// Run the authenticated per-user local companion endpoint.
    Serve {
        #[arg(long)]
        vault_id: String,
        #[arg(long)]
        vault: PathBuf,
        #[arg(long, default_value = "local")]
        grant_id: String,
        #[arg(long, default_value = "vault")]
        scope_id: String,
        #[arg(long)]
        endpoint: Option<PathBuf>,
    },
    /// Expose read-only knowledge tools over MCP stdio through the running companion.
    McpStdio {
        #[arg(long)]
        vault_id: String,
        #[arg(long, default_value = "local")]
        grant_id: String,
        #[arg(long, default_value = "vault")]
        scope_id: String,
        #[arg(long)]
        endpoint: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorReport {
    product_version: &'static str,
    protocol_version: u16,
    platform: &'static str,
    data_directory: PathBuf,
    sqlite_version: String,
    fts5_available: bool,
    credential_store_available: bool,
}

fn doctor() -> Result<DoctorReport> {
    let project_dirs = ProjectDirs::from("dev", "Grimmore", "Grimmore")
        .context("this platform does not expose a per-user application data directory")?;
    let connection = Connection::open_in_memory().context("open diagnostic SQLite database")?;
    let sqlite_version = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .context("read bundled SQLite version")?;
    let fts5_available = connection
        .execute(
            "CREATE VIRTUAL TABLE grimmore_fts_probe USING fts5(body)",
            [],
        )
        .is_ok();
    let credential_store_available = RootSecret::verify_store().is_ok();

    Ok(DoctorReport {
        product_version: env!("CARGO_PKG_VERSION"),
        protocol_version: PROTOCOL_VERSION,
        platform: std::env::consts::OS,
        data_directory: project_dirs.data_local_dir().to_path_buf(),
        sqlite_version,
        fts5_available,
        credential_store_available,
    })
}

fn default_database_path() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("dev", "Grimmore", "Grimmore")
        .context("this platform does not expose a per-user application data directory")?;
    Ok(project_dirs.data_local_dir().join("grimmore.sqlite3"))
}

fn start_initial_vault_index(
    storage: Arc<Storage>,
    vault_id: String,
    vault: PathBuf,
) -> Result<tokio::sync::oneshot::Receiver<Result<()>>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    thread::Builder::new()
        .name("grimmore-initial-index".to_owned())
        .spawn(move || {
            let result = index_vault(&storage, &vault_id, vault)
                .map(|_| ())
                .context("initial vault index");
            let _ = sender.send(result);
        })
        .context("start initial vault index worker")?;
    Ok(receiver)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Doctor => {
            println!("{}", serde_json::to_string_pretty(&doctor()?)?);
        }
        Command::Index { vault_id, vault } => {
            let database = cli.database.map_or_else(default_database_path, Ok)?;
            let storage = Storage::open(&database)
                .with_context(|| format!("open operational database {}", database.display()))?;
            let report = index_vault(&storage, &vault_id, vault)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::ProtocolSchema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&schema_for!(WireContract))?
            );
        }
        Command::Search {
            vault_id,
            query,
            limit,
        } => {
            let database = cli.database.map_or_else(default_database_path, Ok)?;
            let storage = Storage::open(&database)
                .with_context(|| format!("open operational database {}", database.display()))?;
            let result = search_notes(&storage, &vault_id, &query, limit)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Serve {
            vault_id,
            vault,
            grant_id,
            scope_id,
            endpoint,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .with_writer(std::io::stderr)
                .try_init()
                .ok();
            let database = cli.database.map_or_else(default_database_path, Ok)?;
            let storage =
                Arc::new(Storage::open(&database).with_context(|| {
                    format!("open operational database {}", database.display())
                })?);
            let endpoint = endpoint.map_or_else(default_endpoint_path, Ok)?;
            let secret = Arc::new(RootSecret::load_or_create()?);
            let _watcher = VaultWatcher::start(Arc::clone(&storage), vault_id.clone(), &vault)
                .context("start native vault watcher")?;
            let mut initial_index =
                start_initial_vault_index(Arc::clone(&storage), vault_id.clone(), vault)?;
            let server = serve(IpcServerConfig {
                endpoint,
                storage,
                secret,
                vault_id,
                grant_id,
                scope_id,
            });
            tokio::pin!(server);
            tokio::select! {
                result = &mut server => result?,
                result = &mut initial_index => {
                    result.context("initial vault index worker stopped before returning")??;
                    server.await?;
                }
            }
        }
        Command::McpStdio {
            vault_id,
            grant_id,
            scope_id,
            endpoint,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
                )
                .with_writer(std::io::stderr)
                .try_init()
                .ok();
            let endpoint = endpoint.map_or_else(default_endpoint_path, Ok)?;
            serve_stdio(McpStdioConfig {
                endpoint,
                vault_id,
                grant_id,
                scope_id,
            })
            .await?;
        }
    }

    Ok(())
}
