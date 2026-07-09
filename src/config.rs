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
    /// Concurrency of the server's in-process worker — how many `local`-queue
    /// runs it executes at once. From `ORCH_LOCAL_CAPACITY` (default 8). `0`
    /// disables the in-process worker entirely (pure control plane; all
    /// execution goes to remote workers).
    pub local_capacity: u32,
}

impl Config {
    /// Resolve CLI flags into a full config, filling in defaults under
    /// `~/.orchestrator` (created with 0700 permissions if missing).
    ///
    /// A missing `--listen` resolves via [`default_listen`]: `0.0.0.0:$PORT`
    /// when `PORT` is set (Railway/Render/Fly), otherwise `127.0.0.1:4400`.
    ///
    /// Worker tokens come from `--worker-token` (repeatable) plus the
    /// comma-separated `ORCH_WORKER_TOKENS` env var, deduplicated.
    pub fn resolve(
        listen: Option<SocketAddr>,
        db: Option<PathBuf>,
        key: Option<PathBuf>,
        mut worker_tokens: Vec<String>,
    ) -> io::Result<Self> {
        let listen = match listen {
            Some(addr) => addr,
            None => default_listen(std::env::var("PORT").ok().as_deref())?,
        };

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

        // In-process worker concurrency. Invalid values fall back to the
        // default rather than failing startup.
        let local_capacity = std::env::var("ORCH_LOCAL_CAPACITY")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(8);

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
            local_capacity,
        })
    }
}

/// Default listen address when `--listen` is omitted.
///
/// A set `PORT` (the convention on Railway, Render, Fly, Heroku, …) means the
/// process runs behind a platform router and must bind every interface, so we
/// listen on `0.0.0.0:$PORT`. With no `PORT`, we keep the conservative local
/// default of `127.0.0.1:4400` (loopback only). `port` is the raw env value,
/// passed in so this stays a pure, testable function.
fn default_listen(port: Option<&str>) -> io::Result<SocketAddr> {
    match port.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            let port: u16 = p.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid PORT value {p:?} (expected 0-65535)"),
                )
            })?;
            Ok(SocketAddr::from(([0, 0, 0, 0], port)))
        }
        None => Ok(SocketAddr::from(([127, 0, 0, 1], 4400))),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_port_defaults_to_loopback_4400() {
        let addr = default_listen(None).unwrap();
        assert_eq!(addr, "127.0.0.1:4400".parse().unwrap());
    }

    #[test]
    fn set_port_binds_all_interfaces() {
        let addr = default_listen(Some("8080")).unwrap();
        assert_eq!(addr, "0.0.0.0:8080".parse().unwrap());
    }

    #[test]
    fn whitespace_and_empty_port_fall_back_to_default() {
        // A router that exports PORT="" (or padded) should not error; treat it
        // as unset rather than a parse failure.
        assert_eq!(default_listen(Some("")).unwrap(), default_listen(None).unwrap());
        assert_eq!(default_listen(Some("  ")).unwrap(), default_listen(None).unwrap());
        assert_eq!(default_listen(Some(" 3000 ")).unwrap(), "0.0.0.0:3000".parse().unwrap());
    }

    #[test]
    fn non_numeric_port_is_an_error() {
        assert!(default_listen(Some("abc")).is_err());
        assert!(default_listen(Some("70000")).is_err()); // out of u16 range
    }
}
