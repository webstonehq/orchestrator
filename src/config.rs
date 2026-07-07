//! Resolved runtime configuration for the `serve` command.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Fully resolved configuration for `orchestrator serve`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server listens on.
    pub listen: SocketAddr,
    /// Path to the SQLite database file.
    pub db_path: PathBuf,
    /// Path to the master key file for the secrets store.
    pub key_path: PathBuf,
    /// Accepted worker bearer tokens (from `--worker-token` and
    /// `ORCH_WORKER_TOKENS`). Empty disables the worker API.
    pub worker_tokens: Vec<String>,
}

impl Config {
    /// Resolve CLI flags into a full config, filling in defaults under
    /// `~/.orchestrator` (created with 0700 permissions if missing).
    ///
    /// Worker tokens come from `--worker-token` (repeatable) plus the
    /// comma-separated `ORCH_WORKER_TOKENS` env var, deduplicated.
    pub fn resolve(
        listen: SocketAddr,
        db: Option<PathBuf>,
        key: Option<PathBuf>,
        mut worker_tokens: Vec<String>,
    ) -> io::Result<Self> {
        if let Ok(env) = std::env::var("ORCH_WORKER_TOKENS") {
            worker_tokens.extend(
                env.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
            );
        }
        worker_tokens.sort();
        worker_tokens.dedup();

        let (db_path, key_path) = match (db, key) {
            (Some(db), Some(key)) => (db, key),
            (db, key) => {
                let dir = default_dir()?;
                (
                    db.unwrap_or_else(|| dir.join("orchestrator.db")),
                    key.unwrap_or_else(|| dir.join("master.key")),
                )
            }
        };
        Ok(Self {
            listen,
            db_path,
            key_path,
            worker_tokens,
        })
    }
}

/// `~/.orchestrator`, created with 0700 permissions if missing.
pub fn default_dir() -> io::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine home directory",
        )
    })?;
    let dir = home.join(".orchestrator");

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;

    Ok(dir)
}
