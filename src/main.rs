//! Orchestrator: single-binary workflow orchestration tool.
//!
//! One Rust binary hosts the web UI, JSON API, cron scheduler, and task
//! executor. CLI surface in v1 is `orchestrator serve`.

use orchestrator::api::{self, AppState};
use orchestrator::db::Db;
use orchestrator::engine::Engine;
use orchestrator::plugins::PluginRegistry;
use orchestrator::scheduler::{RunLauncher, Scheduler, SystemClock};
use orchestrator::secrets::SecretStore;
use orchestrator::worker as worker_mod;
use orchestrator::{config, ui};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use r2d2_sqlite::SqliteConnectionManager;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "orchestrator", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the web UI, JSON API, scheduler, and executor.
    Serve {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:4400")]
        listen: SocketAddr,

        /// Path to the SQLite database file [default: ~/.orchestrator/orchestrator.db].
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,

        /// Path to the master key file [default: ~/.orchestrator/master.key].
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,

        /// Accept a worker bearer token (repeatable). Also read from the
        /// comma-separated `ORCH_WORKER_TOKENS` env var. With none set, the
        /// worker API is disabled.
        #[arg(long = "worker-token", value_name = "TOKEN")]
        worker_tokens: Vec<String>,
    },

    /// Run as a worker: dial a server, claim queued runs off the given
    /// queues, and execute them locally against your own secrets.
    Worker {
        /// Control-plane base URL, e.g. http://127.0.0.1:4400.
        #[arg(long)]
        server: String,

        /// Bearer token accepted by the server (`serve --worker-token`).
        #[arg(long, env = "ORCH_WORKER_TOKEN")]
        token: String,

        /// Stable id identifying this worker (defaults to the hostname).
        #[arg(long)]
        id: Option<String>,

        /// Comma-separated queues to serve.
        #[arg(long, value_delimiter = ',', default_value = "default")]
        queues: Vec<String>,

        /// Maximum runs executed concurrently.
        #[arg(long, default_value_t = 4)]
        capacity: u32,

        /// Scratch database path [default: ~/.orchestrator/worker.db].
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,

        /// This worker's own secrets key file [default: ~/.orchestrator/worker.key].
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,
    },
}

/// [`RunLauncher`] backed by the engine: the scheduler inserts queued run
/// rows itself, then hands the ids here. `Engine::start`'s run-start input
/// finalization applies defaults, so scheduler runs need no create-time
/// input resolution.
struct EngineLauncher(Arc<Engine>);

impl RunLauncher for EngineLauncher {
    fn launch(&self, run_id: i64) {
        if let Err(e) = self.0.start(run_id) {
            tracing::warn!(run_id, error = %e, "scheduler: failed to start run");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            listen,
            db,
            key,
            worker_tokens,
        } => serve(listen, db, key, worker_tokens).await,
        Command::Worker {
            server,
            token,
            id,
            queues,
            capacity,
            db,
            key,
        } => worker(server, token, id, queues, capacity, db, key).await,
    }
}

async fn serve(
    listen: SocketAddr,
    db: Option<PathBuf>,
    key: Option<PathBuf>,
    worker_tokens: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::resolve(listen, db, key, worker_tokens)?;
    tracing::info!(
        listen = %config.listen,
        db = %config.db_path.display(),
        key = %config.key_path.display(),
        "starting orchestrator"
    );

    // Database (opens/creates the file and applies migrations).
    let db = Db::open(&config.db_path)?;

    // Secrets store. `Db` does not expose its r2d2 pool (only single
    // connections), so the store gets its own small pool on the same file
    // with the same pragmas; WAL + busy_timeout make cross-pool access safe.
    let manager = SqliteConnectionManager::file(&config.db_path).with_init(|conn| {
        // busy_timeout first: the WAL pragma itself needs a lock and races
        // the Db pool's own connection setup at startup.
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    });
    let secrets_pool = r2d2::Pool::builder().max_size(4).build(manager)?;
    let secrets = Arc::new(SecretStore::open(&config.key_path, secrets_pool)?);

    // Plugin registry and engine.
    let registry = Arc::new(PluginRegistry::builtin());
    let engine = Engine::new(db.clone(), Arc::clone(&registry), Arc::clone(&secrets));

    // Startup recovery: runs left queued/running by an unclean shutdown are
    // marked failed ("interrupted by shutdown").
    let recovered = engine.recover_interrupted()?;
    if recovered > 0 {
        tracing::info!("recovered {recovered} interrupted runs");
    }

    // Scheduler: reconcile schedule state at startup, then spawn the loop.
    let scheduler = Scheduler::new(
        db.clone(),
        Arc::new(EngineLauncher(Arc::clone(&engine))),
        Arc::new(SystemClock),
    );
    if let Err(e) = scheduler.reconcile_all() {
        tracing::warn!(error = %e, "startup schedule reconciliation failed");
    }
    let shutdown = CancellationToken::new();
    tokio::spawn(Arc::clone(&scheduler).run(shutdown.clone()));

    // Reaper: fail runs whose worker lease has lapsed (only relevant when
    // workers are enabled, but harmless otherwise).
    if !config.worker_tokens.is_empty() {
        tracing::info!("worker API enabled ({} token(s))", config.worker_tokens.len());
        tokio::spawn(reaper(Arc::clone(&engine), shutdown.clone()));
    }

    // HTTP: JSON API + health + embedded UI (registered routes win over the
    // UI router's fallback).
    let state = AppState {
        db,
        engine: Arc::clone(&engine),
        registry,
        secrets,
        scheduler,
        worker_tokens: Arc::new(config.worker_tokens.clone()),
    };
    let app = Router::new()
        .route("/api/health", get(health))
        .merge(api::router(state))
        .merge(ui::router());

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);

    // Graceful shutdown: ctrl-c cancels the shutdown token (stopping the
    // scheduler loop) and the server drains. Active runs are left as-is;
    // `recover_interrupted` marks their rows failed on the next startup.
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {}
                Err(e) => tracing::warn!(error = %e, "failed to listen for ctrl-c"),
            }
            shutdown.cancel();
            let active = engine.active_run_count();
            tracing::info!(
                "shutting down ({active} runs active — they will be marked interrupted on next startup)"
            );
        })
        .await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok": true}))
}

/// Periodically fail runs whose worker lease has lapsed. Runs until `shutdown`.
async fn reaper(engine: Arc<Engine>, shutdown: CancellationToken) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(15)) => {}
        }
        match engine.reap_lost_runs() {
            Ok(ids) if !ids.is_empty() => {
                tracing::warn!(count = ids.len(), "reaped runs with expired worker leases")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "reaper failed"),
        }
    }
}

/// Entry point for `orchestrator worker`: dial the server and execute claimed
/// runs locally until ctrl-c.
async fn worker(
    server: String,
    token: String,
    id: Option<String>,
    queues: Vec<String>,
    capacity: u32,
    db: Option<PathBuf>,
    key: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let dir = dirs::home_dir()
        .map(|h| h.join(".orchestrator"))
        .unwrap_or_else(|| PathBuf::from("."));
    let worker_id = id
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "worker".to_string());
    let cfg = worker_mod::WorkerConfig {
        server_url: server.trim_end_matches('/').to_string(),
        token,
        worker_id,
        queues,
        capacity,
        db_path: db.unwrap_or_else(|| dir.join("worker.db")),
        key_path: key.unwrap_or_else(|| dir.join("worker.key")),
    };

    let shutdown = CancellationToken::new();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.cancel();
    });
    worker_mod::run(cfg, shutdown)
        .await
        .map_err(|e| e as Box<dyn std::error::Error>)
}
