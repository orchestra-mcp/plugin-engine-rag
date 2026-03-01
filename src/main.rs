//! Orchestra RAG engine plugin binary.
//!
//! A Rust plugin that communicates with the Orchestra orchestrator over QUIC + mTLS.
//! Provides code parsing (Tree-sitter), full-text search (Tantivy), and
//! RAG memory services.
//!
//! Usage:
//!   orchestra-rag --orchestrator-addr <addr> --listen-addr localhost:0 --certs-dir <dir> --workspace <path>
//!   orchestra-rag --manifest   # Print JSON manifest and exit

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};

use orchestra_rag::db::DbPool;
use orchestra_rag::memory::schema::MemorySchema;
use orchestra_rag::protocol::handler::RequestHandler;
use orchestra_rag::protocol::server::PluginServer;
use orchestra_rag::tools::{self, ToolRegistry};

/// Orchestra RAG Engine Plugin
///
/// Provides code parsing, search indexing, and RAG memory over QUIC + mTLS.
#[derive(Parser, Debug)]
#[command(name = "orchestra-rag", version, about)]
struct Cli {
    /// Address of the orchestrator to connect to (e.g. 127.0.0.1:9090)
    #[arg(long, default_value = "127.0.0.1:9090")]
    orchestrator_addr: String,

    /// Address to listen on for incoming QUIC connections (use port 0 for auto).
    /// Accepts hostnames like "localhost:0" as well as numeric addresses.
    #[arg(long, default_value = "127.0.0.1:0")]
    listen_addr: String,

    /// Directory containing TLS certificates (server.crt, server.key)
    #[arg(long)]
    certs_dir: Option<PathBuf>,

    /// Workspace root path for indexing and parsing
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Print the plugin manifest as JSON and exit
    #[arg(long)]
    manifest: bool,
}

/// The plugin manifest as a JSON-serializable struct.
/// Matches the PluginManifest protobuf message fields.
#[derive(serde::Serialize)]
struct ManifestJson {
    id: String,
    version: String,
    language: String,
    description: String,
    author: String,
    binary: String,
    provides_tools: Vec<String>,
    provides_events: Vec<String>,
    provides_storage: Vec<String>,
    needs_storage: Vec<String>,
    needs_events: Vec<String>,
}

fn build_manifest() -> ManifestJson {
    ManifestJson {
        id: "engine.rag".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        language: "rust".to_string(),
        description: "Code parsing, search indexing, and RAG memory engine".to_string(),
        author: "Orchestra".to_string(),
        binary: "orchestra-rag".to_string(),
        provides_tools: vec![
            "health_check".to_string(),
            "parse_file".to_string(),
            "get_symbols".to_string(),
            "get_imports".to_string(),
            "index_file".to_string(),
            "search".to_string(),
            "delete_from_index".to_string(),
            "clear_index".to_string(),
            "get_index_stats".to_string(),
            "search_symbols".to_string(),
            "index_directory".to_string(),
            "save_memory".to_string(),
            "search_memory".to_string(),
            "get_context".to_string(),
            "list_memories".to_string(),
            "get_memory".to_string(),
            "update_memory".to_string(),
            "delete_memory".to_string(),
            "save_observation".to_string(),
            "get_project_summary".to_string(),
            "start_session".to_string(),
            "end_session".to_string(),
            // LSP tools
            "lsp_open_document".to_string(),
            "lsp_close_document".to_string(),
            "lsp_update_document".to_string(),
            "lsp_goto_definition".to_string(),
            "lsp_find_references".to_string(),
            "lsp_hover".to_string(),
            "lsp_complete".to_string(),
            "lsp_diagnostics".to_string(),
            "lsp_workspace_symbols".to_string(),
            "lsp_build_index".to_string(),
        ],
        provides_events: vec![
            "file.indexed".to_string(),
            "memory.saved".to_string(),
        ],
        provides_storage: Vec::new(),
        needs_storage: vec!["markdown".to_string()],
        needs_events: vec!["file.changed".to_string()],
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // If --manifest flag is set, print manifest JSON and exit
    if cli.manifest {
        let manifest = build_manifest();
        let json = serde_json::to_string_pretty(&manifest)
            .context("failed to serialize manifest")?;
        println!("{json}");
        return Ok(());
    }

    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();

    info!(
        workspace = %cli.workspace.display(),
        listen_addr = %cli.listen_addr,
        orchestrator_addr = %cli.orchestrator_addr,
        "starting orchestra-rag engine"
    );

    // Resolve workspace to absolute path
    let workspace = cli
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| cli.workspace.clone());

    // Initialize the local SQLite database for memory services
    let db_path = workspace.join(".orchestra").join("rag.db");
    let pool = DbPool::new(db_path.clone())
        .context("failed to create database pool")?;
    pool.with_connection(|conn| {
        MemorySchema::init(conn).map_err(|e| {
            orchestra_rag::db::pool::DbError::Pool(format!("schema init failed: {e}"))
        })
    })
    .context("failed to initialize memory schema")?;
    info!(db = %db_path.display(), "database initialized");

    // Initialize LSP SQLite database (separate table space in the same db)
    let lsp_pool = DbPool::new(db_path.clone())
        .context("failed to create LSP database pool")?;

    // Build the tool registry
    let index_path = workspace.join(".orchestra").join("index");
    let mut registry = ToolRegistry::new();
    tools::register_all_tools_with_lsp(
        &mut registry,
        Some(index_path),
        Some(pool),
        Some(lsp_pool),
    );
    info!(tools = registry.tool_count(), "tools registered");

    let registry = Arc::new(registry);

    // Build the request handler
    let handler = RequestHandler::new(Arc::clone(&registry));

    // Resolve listen address (supports hostnames like "localhost:0").
    let listen_addr: SocketAddr = cli
        .listen_addr
        .to_socket_addrs()
        .context("failed to resolve listen address")?
        .next()
        .context("listen address resolved to no addresses")?;

    // Build and start the QUIC server
    let server = PluginServer::new(handler, listen_addr, cli.certs_dir);

    // Set up graceful shutdown on SIGINT / SIGTERM
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("failed to install SIGINT handler");
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        tokio::select! {
            _ = sigint.recv() => {
                warn!("received SIGINT, initiating shutdown");
            }
            _ = sigterm.recv() => {
                warn!("received SIGTERM, initiating shutdown");
            }
        }
        cancel_clone.cancel();
    });

    // Start the server (blocks until cancelled)
    let _addr = server
        .listen_and_serve(cancel)
        .await
        .context("QUIC server failed")?;

    info!(workspace = %workspace.display(), "orchestra-rag engine shut down cleanly");
    Ok(())
}
