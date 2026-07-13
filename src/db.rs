//! SQLite connection pool and schema migrations.
//!
//! Wraps an r2d2 pool over rusqlite with per-connection pragmas
//! (`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`,
//! `busy_timeout=5000`), an embedded migration runner, and typed query helpers
//! used by the engine, scheduler, and API layers.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, ToSql, TransactionBehavior, params};
use serde::Serialize;

/// Errors from the DB layer: pool checkout or SQLite failures.
#[derive(Debug)]
pub enum DbError {
    Pool(r2d2::Error),
    Sqlite(rusqlite::Error),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Pool(e) => write!(f, "connection pool error: {e}"),
            DbError::Sqlite(e) => write!(f, "sqlite error: {e}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Pool(e) => Some(e),
            DbError::Sqlite(e) => Some(e),
        }
    }
}

impl From<r2d2::Error> for DbError {
    fn from(e: r2d2::Error) -> Self {
        DbError::Pool(e)
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

pub type DbResult<T> = Result<T, DbError>;

/// Current UTC time as an RFC3339 string (millisecond precision, `Z` suffix).
/// All timestamps in the database use this format.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Whether a `runs.status` value is terminal — the run has finished and its
/// row must never be mutated again (`success`/`degraded`/`failed`/`canceled`,
/// per the `runs.status` schema comment).
pub fn is_terminal_run_status(status: &str) -> bool {
    matches!(status, "success" | "degraded" | "failed" | "canceled")
}

/// An RFC3339 timestamp `lease_secs` in the future, in the storage format.
fn lease_deadline(lease_secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(lease_secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

const MIGRATION_001: &str = r#"
CREATE TABLE flows (
  id TEXT PRIMARY KEY,              -- slug, e.g. "council-alert-pipeline"
  name TEXT NOT NULL,
  namespace TEXT NOT NULL DEFAULT 'default',
  description TEXT NOT NULL DEFAULT '',
  definition TEXT NOT NULL,         -- JSON (FlowDefinition)
  current_rev INTEGER NOT NULL DEFAULT 1,
  paused INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,         -- RFC3339 UTC, everywhere below too
  updated_at TEXT NOT NULL
);
CREATE TABLE flow_revisions (
  flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
  rev INTEGER NOT NULL,
  definition TEXT NOT NULL,
  message TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  PRIMARY KEY (flow_id, rev)
);
CREATE TABLE runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  flow_id TEXT NOT NULL,
  flow_rev INTEGER NOT NULL,
  status TEXT NOT NULL,             -- queued|running|success|degraded|failed|canceled
  trigger TEXT NOT NULL,            -- schedule|manual|api
  inputs TEXT NOT NULL,             -- JSON object
  scheduled_for TEXT,               -- cron occurrence covered (catch-up runs)
  started_at TEXT, finished_at TEXT,
  error TEXT
);
CREATE INDEX idx_runs_flow ON runs(flow_id, id DESC);
CREATE INDEX idx_runs_status ON runs(status);
CREATE TABLE task_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL,
  status TEXT NOT NULL,             -- pending|running|success|failed|canceled|skipped
  attempt INTEGER NOT NULL DEFAULT 0,
  result TEXT,                      -- JSON, secret-redacted
  outputs TEXT,                     -- JSON object of extracted outputs
  error TEXT,
  started_at TEXT, finished_at TEXT,
  UNIQUE (run_id, task_id)
);
CREATE TABLE task_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_run_id INTEGER NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  item TEXT NOT NULL,               -- JSON of the fan-out element, redacted
  status TEXT NOT NULL,             -- queued|running|success|failed|canceled|dropped
  attempt INTEGER NOT NULL DEFAULT 0,
  result TEXT, error TEXT,
  started_at TEXT, finished_at TEXT,
  UNIQUE (task_run_id, idx)
);
CREATE INDEX idx_items_status ON task_run_items(task_run_id, status);
CREATE TABLE logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL,
  ts TEXT NOT NULL,
  level TEXT NOT NULL,              -- INFO|OK|WARN|ERR|DBG
  task TEXT NOT NULL DEFAULT 'flow',
  message TEXT NOT NULL
);
CREATE INDEX idx_logs_run ON logs(run_id, id);
CREATE TABLE schedule_state (
  flow_id TEXT NOT NULL,
  trigger_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  next_fire_at TEXT,
  last_fired_at TEXT,
  PRIMARY KEY (flow_id, trigger_id)
);
CREATE TABLE secrets (
  name TEXT PRIMARY KEY,
  ciphertext BLOB NOT NULL,         -- 12-byte nonce || chacha20poly1305 ct
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#;

/// Adds the `queue` routing column to runs: which execution queue a run is
/// dispatched to. `local` (the default) is served by the server's in-process
/// executor; any other value waits for a worker subscribed to that queue.
const MIGRATION_002: &str = r#"
ALTER TABLE runs ADD COLUMN queue TEXT NOT NULL DEFAULT 'local';
CREATE INDEX idx_runs_queue ON runs(queue, status);
"#;

/// Adds worker-lease bookkeeping to runs: a run claimed by a remote worker is
/// `leased` with the claiming `worker_id` and a `lease_expires_at` deadline
/// the worker renews via heartbeats; a lapsed lease is reaped. `last_seq` is
/// the highest per-run update sequence number the server has applied, so a
/// reconnecting worker's replayed updates are idempotent.
const MIGRATION_003: &str = r#"
ALTER TABLE runs ADD COLUMN worker_id TEXT;
ALTER TABLE runs ADD COLUMN lease_expires_at TEXT;
ALTER TABLE runs ADD COLUMN last_seq INTEGER NOT NULL DEFAULT 0;
"#;

/// Adds `created_at` to task_runs: when the task run row was first created
/// (enqueued), stamped ahead of `started_at` (when it began running). The gap
/// is the task's assignment/queue latency — the metric the Windmill benchmark
/// reports. Backfilled for pre-existing rows from the best timestamp available.
const MIGRATION_004: &str = r#"
ALTER TABLE task_runs ADD COLUMN created_at TEXT;
UPDATE task_runs SET created_at = COALESCE(started_at, finished_at);
"#;

/// Human authentication: `users` (username + argon2id hash) and server-side
/// `sessions` (opaque token → user, with an expiry). Empty until an admin is
/// seeded from env at startup; distinct from the worker bearer-token scheme.
const MIGRATION_005: &str = r#"
CREATE TABLE users (
  id         INTEGER PRIMARY KEY,
  username   TEXT NOT NULL UNIQUE,
  pw_hash    TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE sessions (
  token      TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
"#;

/// Adds `attempt` to runs: how many times this run has been (re)dispatched.
/// `0` is the first attempt. Incremented when the reaper requeues a run whose
/// worker was lost, up to the flow's `on_worker_loss.max_attempts`. Existing
/// rows backfill to 0.
const MIGRATION_006: &str = r#"
ALTER TABLE runs ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;
"#;

/// Embedded migrations, applied in order; versions recorded in `migrations`.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, MIGRATION_001),
    (2, MIGRATION_002),
    (3, MIGRATION_003),
    (4, MIGRATION_004),
    (5, MIGRATION_005),
    (6, MIGRATION_006),
];

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

/// Mirrors storage, not API shape — parse JSON text columns (`definition`)
/// before serializing into API responses.
#[derive(Debug, Clone, Serialize)]
pub struct FlowRow {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub description: String,
    pub definition: String,
    pub current_rev: i64,
    pub paused: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Mirrors storage, not API shape — parse JSON text columns (`definition`)
/// before serializing into API responses.
#[derive(Debug, Clone, Serialize)]
pub struct FlowRevisionRow {
    pub flow_id: String,
    pub rev: i64,
    pub definition: String,
    pub message: String,
    pub created_at: String,
}

/// Mirrors storage, not API shape — parse JSON text columns (`inputs`)
/// before serializing into API responses.
#[derive(Debug, Clone, Serialize)]
pub struct RunRow {
    pub id: i64,
    pub flow_id: String,
    pub flow_rev: i64,
    pub status: String,
    pub trigger: String,
    pub inputs: String,
    /// Execution queue this run is dispatched to (`local` = in-process).
    pub queue: String,
    pub scheduled_for: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    /// Worker that leased this run (`None` for local/unclaimed runs).
    pub worker_id: Option<String>,
    /// Lease deadline a claiming worker renews via heartbeats.
    pub lease_expires_at: Option<String>,
    /// Highest per-run update sequence number applied (worker reporting).
    pub last_seq: i64,
    /// Dispatch attempt (0 = first). Bumped when the reaper requeues a run
    /// whose worker was lost, up to the flow's `on_worker_loss.max_attempts`.
    pub attempt: i64,
}

/// Mirrors storage, not API shape — parse JSON text columns (`result`,
/// `outputs`) before serializing into API responses.
#[derive(Debug, Clone, Serialize)]
pub struct TaskRunRow {
    pub id: i64,
    pub run_id: i64,
    pub task_id: String,
    pub status: String,
    pub attempt: i64,
    pub result: Option<String>,
    pub outputs: Option<String>,
    pub error: Option<String>,
    /// When the task run row was first created (enqueued); stamped ahead of
    /// `started_at`. The `created_at` → `started_at` gap is assignment latency.
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// Mirrors storage, not API shape — parse JSON text columns (`item`,
/// `result`) before serializing into API responses.
#[derive(Debug, Clone, Serialize)]
pub struct ItemRow {
    pub id: i64,
    pub task_run_id: i64,
    pub idx: i64,
    pub item: String,
    pub status: String,
    pub attempt: i64,
    pub result: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ItemAggregates {
    pub total: u64,
    pub queued: u64,
    pub running: u64,
    pub success: u64,
    pub failed: u64,
    pub dropped: u64,
    /// Rows with attempt > 1.
    pub retried: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogRow {
    pub id: i64,
    pub run_id: i64,
    pub ts: String,
    pub level: String,
    pub task: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleRow {
    pub flow_id: String,
    pub flow_name: String,
    pub flow_paused: bool,
    pub trigger_id: String,
    pub enabled: bool,
    pub next_fire_at: Option<String>,
    pub last_fired_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Runs24h {
    pub total: u64,
    pub ok: u64,
    pub degraded: u64,
    pub failed: u64,
    pub running: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DashboardMetrics {
    /// Flows not paused.
    pub active_flows: u64,
    pub runs_24h: Runs24h,
    /// success / (success + degraded + failed) over runs finished in the last
    /// 30 days; `degraded` runs lower the rate without counting as successes.
    /// `None` when no runs finished in the window. Fraction in `0.0..=1.0`.
    pub success_rate_30d: Option<f64>,
    pub avg_duration_sec_30d: Option<f64>,
}

/// Per-flow run statistics for the flows list and schedules screens.
/// Flows that have never run get no map entry; use
/// [`FlowRunStats::default`] for those.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FlowRunStats {
    /// Status of the most recent run (by id), any status.
    pub last_run_status: Option<String>,
    pub last_run_finished_at: Option<String>,
    /// success / (success + degraded + failed) over runs finished in the last
    /// 30 days; `degraded` runs lower the rate without counting as successes.
    /// `None` when no runs finished in the window. Fraction in `0.0..=1.0`.
    pub success_rate_30d: Option<f64>,
    pub avg_duration_sec_30d: Option<f64>,
}

// ---------------------------------------------------------------------------
// Update parameter structs
// ---------------------------------------------------------------------------

/// Fields for [`Db::update_run_status`]. `error` is always written (pass
/// `None` to clear); `started_at` / `finished_at` are only written when
/// `Some` and preserved otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunStatusUpdate<'a> {
    pub status: &'a str,
    pub error: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub finished_at: Option<&'a str>,
}

/// Fields for [`Db::finish_task_run`]. `result` / `outputs` / `error` are
/// always written; `finished_at` is stamped with now by the helper.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskRunFinish<'a> {
    pub status: &'a str,
    pub result: Option<&'a str>,
    pub outputs: Option<&'a str>,
    pub error: Option<&'a str>,
}

/// Fields for [`Db::update_item`]. `result` / `error` are always written;
/// `started_at` / `finished_at` are only written when `Some` and preserved
/// otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct ItemUpdate<'a> {
    pub status: &'a str,
    pub attempt: i64,
    pub result: Option<&'a str>,
    pub error: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub finished_at: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Db
// ---------------------------------------------------------------------------

type Pool = r2d2::Pool<SqliteConnectionManager>;
type PooledConn = PooledConnection<SqliteConnectionManager>;

/// SQLite connection pool with schema migrations applied.
#[derive(Clone)]
pub struct Db {
    pool: Pool,
}

impl Db {
    /// Open (creating if needed) the database at `path`, configure pragmas on
    /// every pooled connection, and apply pending migrations.
    pub fn open<P: AsRef<Path>>(path: P) -> DbResult<Self> {
        let manager = SqliteConnectionManager::file(path.as_ref()).with_init(|conn| {
            // busy_timeout first: the WAL conversion below takes a lock, and
            // concurrent pool connections race it on first-ever startup.
            conn.busy_timeout(Duration::from_millis(5000))?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            // synchronous=NORMAL under WAL: commits append to the WAL without an
            // fsync, which only happens at checkpoint. This is the single
            // biggest write-throughput lever (no per-commit fsync), and it is
            // safe here — a power/OS crash can lose transactions committed since
            // the last checkpoint but never corrupts the database, and runs
            // in-flight at an unclean shutdown are resolved (requeued or failed)
            // by startup recovery (see `Engine::recover_on_startup`), so losing
            // their last few status writes changes nothing observable.
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            Ok(())
        });
        let pool = r2d2::Pool::builder().build(manager)?;
        let db = Db { pool };
        let mut conn = db.conn()?;
        migrate(&mut conn)?;
        Ok(db)
    }

    /// Check out a raw pooled connection (e.g. for the secrets store).
    pub fn conn(&self) -> DbResult<PooledConn> {
        Ok(self.pool.get()?)
    }

    // -- auth: users & sessions ---------------------------------------------

    /// Whether any user account exists. Drives the first-run onboarding gate:
    /// an empty table means the setup screen is offered until an admin is made.
    pub fn has_users(&self) -> DbResult<bool> {
        let conn = self.conn()?;
        Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |r| r.get(0))?)
    }

    /// Create the first (admin) user, but only while the table is empty.
    /// Returns the new id, or `None` if a user already exists (setup already
    /// done). Uses an IMMEDIATE transaction so two concurrent setup attempts
    /// can't both win the empty-table race.
    pub fn create_first_user(&self, username: &str, pw_hash: &str) -> DbResult<Option<i64>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |r| r.get(0))?;
        if exists {
            return Ok(None);
        }
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO users (username, pw_hash, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?3)",
            params![username, pw_hash, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(id))
    }

    /// The id of a user by username, if they exist.
    pub fn get_user_id(&self, username: &str) -> DbResult<Option<i64>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT id FROM users WHERE username = ?1",
                [username],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The stored argon2 hash for a username, if the user exists.
    pub fn get_user_hash(&self, username: &str) -> DbResult<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT pw_hash FROM users WHERE username = ?1",
                [username],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Create a session valid for `ttl_secs` from now.
    pub fn create_session(&self, token: &str, user_id: i64, ttl_secs: i64) -> DbResult<()> {
        let conn = self.conn()?;
        let now = chrono::Utc::now();
        let created = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let expires = (now + chrono::Duration::seconds(ttl_secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        conn.execute(
            "INSERT INTO sessions (token, user_id, created_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![token, user_id, created, expires],
        )?;
        Ok(())
    }

    /// Username for a live (unexpired) session token, if any.
    pub fn session_username(&self, token: &str) -> DbResult<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT u.username FROM sessions s JOIN users u ON u.id = s.user_id \
                 WHERE s.token = ?1 AND s.expires_at > ?2",
                params![token, now_rfc3339()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Delete a session (logout). Ok whether or not the row existed.
    pub fn delete_session(&self, token: &str) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM sessions WHERE token = ?1", [token])?;
        Ok(())
    }

    /// Delete all expired session rows (opportunistic cleanup at login).
    pub fn sweep_expired_sessions(&self) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM sessions WHERE expires_at < ?1",
            [now_rfc3339()],
        )?;
        Ok(())
    }

    // -- flows --------------------------------------------------------------

    /// Insert or update a flow and record a new revision, in one transaction.
    /// Returns the new revision number (1 on create). `created_at` and
    /// `paused` are preserved on update.
    pub fn upsert_flow_with_revision(
        &self,
        id: &str,
        name: &str,
        namespace: &str,
        description: &str,
        definition_json: &str,
        message: &str,
    ) -> DbResult<i64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let now = now_rfc3339();
        let current: Option<i64> = tx
            .query_row("SELECT current_rev FROM flows WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let rev = match current {
            None => {
                tx.execute(
                    "INSERT INTO flows (id, name, namespace, description, definition, \
                     current_rev, paused, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, ?6, ?6)",
                    params![id, name, namespace, description, definition_json, now],
                )?;
                1
            }
            Some(cur) => {
                let rev = cur + 1;
                tx.execute(
                    "UPDATE flows SET name = ?2, namespace = ?3, description = ?4, \
                     definition = ?5, current_rev = ?6, updated_at = ?7 WHERE id = ?1",
                    params![id, name, namespace, description, definition_json, rev, now],
                )?;
                rev
            }
        };
        tx.execute(
            "INSERT INTO flow_revisions (flow_id, rev, definition, message, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, rev, definition_json, message, now],
        )?;
        tx.commit()?;
        Ok(rev)
    }

    pub fn get_flow(&self, id: &str) -> DbResult<Option<FlowRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT id, name, namespace, description, definition, current_rev, paused, \
                 created_at, updated_at FROM flows WHERE id = ?1",
                [id],
                map_flow,
            )
            .optional()?)
    }

    pub fn list_flows(&self) -> DbResult<Vec<FlowRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, namespace, description, definition, current_rev, paused, \
             created_at, updated_at FROM flows ORDER BY namespace, name",
        )?;
        let rows = stmt
            .query_map([], map_flow)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete a flow (revisions cascade) and its schedule state. Returns
    /// whether a flow row was deleted. Run history is kept.
    pub fn delete_flow(&self, id: &str) -> DbResult<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM schedule_state WHERE flow_id = ?1", [id])?;
        let n = tx.execute("DELETE FROM flows WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn set_paused(&self, id: &str, paused: bool) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE flows SET paused = ?2 WHERE id = ?1",
            params![id, paused],
        )?;
        Ok(())
    }

    /// Revisions for a flow, newest first.
    pub fn list_revisions(&self, id: &str) -> DbResult<Vec<FlowRevisionRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT flow_id, rev, definition, message, created_at \
             FROM flow_revisions WHERE flow_id = ?1 ORDER BY rev DESC",
        )?;
        let rows = stmt
            .query_map([id], map_revision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Seed a flow + one revision at a specific `rev` — used by a worker's
    /// scratch database so the engine's definition lookup finds the assigned
    /// flow. Idempotent: re-seeding an assignment overwrites the definition.
    pub fn seed_flow_revision(
        &self,
        flow_id: &str,
        rev: i64,
        name: &str,
        definition: &str,
    ) -> DbResult<()> {
        let conn = self.conn()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO flows (id, name, namespace, description, definition, current_rev, \
             paused, created_at, updated_at) VALUES (?1, ?2, 'default', '', ?3, ?4, 0, ?5, ?5) \
             ON CONFLICT(id) DO UPDATE SET definition = excluded.definition, \
             current_rev = excluded.current_rev, updated_at = excluded.updated_at",
            params![flow_id, name, definition, rev, now],
        )?;
        conn.execute(
            "INSERT INTO flow_revisions (flow_id, rev, definition, message, created_at) \
             VALUES (?1, ?2, ?3, 'assigned', ?4) \
             ON CONFLICT(flow_id, rev) DO UPDATE SET definition = excluded.definition",
            params![flow_id, rev, definition, now],
        )?;
        Ok(())
    }

    pub fn get_revision(&self, id: &str, rev: i64) -> DbResult<Option<FlowRevisionRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT flow_id, rev, definition, message, created_at \
                 FROM flow_revisions WHERE flow_id = ?1 AND rev = ?2",
                params![id, rev],
                map_revision,
            )
            .optional()?)
    }

    // -- runs ---------------------------------------------------------------

    /// Insert a run with status `queued` on the given `queue`. Returns the
    /// run id.
    pub fn insert_run(
        &self,
        flow_id: &str,
        flow_rev: i64,
        trigger: &str,
        inputs_json: &str,
        queue: &str,
        scheduled_for: Option<&str>,
    ) -> DbResult<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO runs (flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for) \
             VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6)",
            params![
                flow_id,
                flow_rev,
                trigger,
                inputs_json,
                queue,
                scheduled_for
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_run(&self, id: i64) -> DbResult<Option<RunRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT id, flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for, \
                 started_at, finished_at, error, worker_id, lease_expires_at, last_seq, attempt FROM runs WHERE id = ?1",
                [id],
                map_run,
            )
            .optional()?)
    }

    /// List runs newest-first with optional filters. `since`/`until` bound
    /// `started_at` (RFC3339, half-open `[since, until)`). `page` is 1-based.
    /// Returns the page of rows and the total matching count.
    pub fn list_runs(
        &self,
        flow: Option<&str>,
        status: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        page: u32,
        per: u32,
    ) -> DbResult<(Vec<RunRow>, u64)> {
        let conn = self.conn()?;
        let mut where_sql = String::from("1=1");
        let mut filters: Vec<&dyn ToSql> = Vec::new();
        if let Some(f) = flow.as_ref() {
            where_sql.push_str(" AND flow_id = ?");
            filters.push(f);
        }
        if let Some(s) = status.as_ref() {
            where_sql.push_str(" AND status = ?");
            filters.push(s);
        }
        if let Some(t) = trigger.as_ref() {
            where_sql.push_str(" AND trigger = ?");
            filters.push(t);
        }
        if let Some(s) = since.as_ref() {
            where_sql.push_str(" AND started_at >= ?");
            filters.push(s);
        }
        if let Some(u) = until.as_ref() {
            where_sql.push_str(" AND started_at < ?");
            filters.push(u);
        }

        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM runs WHERE {where_sql}"),
            &filters[..],
            |r| r.get(0),
        )?;

        let limit = i64::from(per);
        let offset = i64::from(page.max(1) - 1) * limit;
        let mut params_vec = filters;
        params_vec.push(&limit);
        params_vec.push(&offset);
        let mut stmt = conn.prepare(&format!(
            "SELECT id, flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for, \
             started_at, finished_at, error, worker_id, lease_expires_at, last_seq, attempt FROM runs WHERE {where_sql} \
             ORDER BY id DESC LIMIT ? OFFSET ?"
        ))?;
        let rows = stmt
            .query_map(&params_vec[..], map_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total as u64))
    }

    /// Count of runs per status (for the filter chips).
    pub fn count_runs_by_status(&self) -> DbResult<HashMap<String, u64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM runs GROUP BY status")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Whether `run_id` has reached a durably terminal status (see
    /// [`is_terminal_run_status`]). A missing run counts as non-terminal.
    ///
    /// Used to fence out late worker updates: once the reaper (or a normal
    /// finish) has settled a run, a worker that reconnects after being reaped
    /// must not be able to flush its buffered updates and resurrect it.
    pub fn run_is_terminal(&self, run_id: i64) -> DbResult<bool> {
        let conn = self.conn()?;
        let status: Option<String> = conn
            .query_row("SELECT status FROM runs WHERE id = ?1", [run_id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(status.as_deref().is_some_and(is_terminal_run_status))
    }

    /// Whether `worker_id` is the current lease owner of `run_id`. Fences out
    /// updates from a worker that was reaped and replaced: once a lost run has
    /// been requeued and re-claimed by another worker (or cleared to no owner),
    /// the stale worker's buffered updates must not land on the new attempt.
    /// A missing run is not owned by anyone.
    pub fn worker_owns_run(&self, worker_id: &str, run_id: i64) -> DbResult<bool> {
        let conn = self.conn()?;
        let owner: Option<Option<String>> = conn
            .query_row("SELECT worker_id FROM runs WHERE id = ?1", [run_id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(matches!(owner, Some(Some(w)) if w == worker_id))
    }

    /// Update a run's status and error. See [`RunStatusUpdate`] for which
    /// fields preserve existing values when `None`.
    pub fn update_run_status(&self, id: i64, update: RunStatusUpdate<'_>) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE runs SET status = ?2, error = ?3, \
             started_at = COALESCE(?4, started_at), \
             finished_at = COALESCE(?5, finished_at) WHERE id = ?1",
            params![
                id,
                update.status,
                update.error,
                update.started_at,
                update.finished_at
            ],
        )?;
        Ok(())
    }

    // -- worker leasing -------------------------------------------------------

    /// Atomically lease up to `capacity` `queued` runs on any of `queues` to
    /// `worker_id`, flipping them to `leased` with a `lease_expires_at` of
    /// `now + lease_secs`. Returns the claimed rows (ascending id).
    ///
    /// The whole claim runs in a single `IMMEDIATE` transaction (write lock
    /// taken up front), so two workers claiming the same queue concurrently
    /// can never lease the same run — the second sees the rows already gone
    /// from `status = 'queued'`.
    pub fn claim_runs(
        &self,
        worker_id: &str,
        queues: &[&str],
        capacity: u32,
        lease_secs: i64,
    ) -> DbResult<Vec<RunRow>> {
        if queues.is_empty() || capacity == 0 {
            return Ok(Vec::new());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let placeholders = queues.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let limit = i64::from(capacity);
        let ids: Vec<i64> = {
            let mut params: Vec<&dyn ToSql> = queues.iter().map(|q| q as &dyn ToSql).collect();
            params.push(&limit);
            let mut stmt = tx.prepare(&format!(
                "SELECT id FROM runs WHERE status = 'queued' AND queue IN ({placeholders}) \
                 ORDER BY id LIMIT ?"
            ))?;
            stmt.query_map(&params[..], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if ids.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }

        let lease_expires = lease_deadline(lease_secs);
        for id in &ids {
            tx.execute(
                "UPDATE runs SET status = 'leased', worker_id = ?2, lease_expires_at = ?3 \
                 WHERE id = ?1",
                params![id, worker_id, lease_expires],
            )?;
        }

        let id_list = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let rows = {
            let mut stmt = tx.prepare(&format!(
                "SELECT id, flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for, \
                 started_at, finished_at, error, worker_id, lease_expires_at, last_seq, attempt FROM runs WHERE id IN ({id_list}) ORDER BY id"
            ))?;
            stmt.query_map([], map_run)?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(rows)
    }

    /// Atomically lease one specific `queued` run to `worker_id` (a targeted
    /// claim, e.g. the in-process worker starting a named run). Returns the
    /// leased row, or `None` if it was not `queued` — already claimed or
    /// finished (the caller lost the race). Same lease semantics as
    /// [`Db::claim_runs`].
    pub fn claim_run(
        &self,
        run_id: i64,
        worker_id: &str,
        lease_secs: i64,
    ) -> DbResult<Option<RunRow>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE runs SET status = 'leased', worker_id = ?2, lease_expires_at = ?3 \
             WHERE id = ?1 AND status = 'queued'",
            params![run_id, worker_id, lease_deadline(lease_secs)],
        )?;
        if n == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let row = tx.query_row(
            "SELECT id, flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for, \
             started_at, finished_at, error, worker_id, lease_expires_at, last_seq, attempt \
             FROM runs WHERE id = ?1",
            [run_id],
            map_run,
        )?;
        tx.commit()?;
        Ok(Some(row))
    }

    /// Extend the lease deadline of `worker_id`'s active runs to
    /// `now + lease_secs`. Only rows still `leased`/`running` and owned by
    /// this worker are touched. Returns the number of rows renewed.
    pub fn renew_leases(&self, worker_id: &str, run_ids: &[i64], lease_secs: i64) -> DbResult<u64> {
        if run_ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn()?;
        let lease_expires = lease_deadline(lease_secs);
        let placeholders = run_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut params: Vec<&dyn ToSql> = vec![&worker_id, &lease_expires];
        for id in run_ids {
            params.push(id);
        }
        let n = conn.execute(
            &format!(
                "UPDATE runs SET lease_expires_at = ?2 \
                 WHERE worker_id = ?1 AND status IN ('leased', 'running') \
                 AND id IN ({placeholders})"
            ),
            &params[..],
        )?;
        Ok(n as u64)
    }

    /// Full rows of runs whose worker lease has lapsed (`leased`/`running`
    /// with `lease_expires_at < now`). The reaper reads these, then decides
    /// per run whether to requeue for a fresh attempt or fail, via
    /// [`Db::requeue_lost_run`] / [`Db::fail_lost_run`].
    pub fn expired_lease_runs(&self, now: &str) -> DbResult<Vec<RunRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for, \
             started_at, finished_at, error, worker_id, lease_expires_at, last_seq, attempt \
             FROM runs WHERE status IN ('leased', 'running') \
             AND lease_expires_at IS NOT NULL AND lease_expires_at < ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([now], map_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Fail a run whose worker was lost: the run and its unfinished task_runs
    /// are failed ("worker lost (lease expired)") and their items canceled.
    /// Guarded on the run still being
    /// `leased`/`running`, so a run that finished or was canceled between
    /// selection and here is left alone. Returns whether it acted.
    pub fn fail_lost_run(&self, id: i64, now: &str) -> DbResult<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE runs SET status = 'failed', error = 'worker lost (lease expired)', \
             finished_at = COALESCE(finished_at, ?2) \
             WHERE id = ?1 AND status IN ('leased', 'running')",
            params![id, now],
        )?;
        if n == 0 {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "UPDATE task_runs SET status = 'failed', error = 'worker lost (lease expired)', \
             finished_at = COALESCE(finished_at, ?2) \
             WHERE run_id = ?1 AND status IN ('pending', 'queued', 'running')",
            params![id, now],
        )?;
        tx.execute(
            "UPDATE task_run_items SET status = 'canceled', \
             finished_at = COALESCE(finished_at, ?2) \
             WHERE task_run_id IN (SELECT id FROM task_runs WHERE run_id = ?1) \
             AND status IN ('queued', 'running')",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Requeue a run whose worker was lost, for a fresh attempt: reset it to
    /// `queued` with `attempt = next_attempt`, clear the lease/worker/seq and
    /// prior timing, and delete the previous attempt's task_runs (items
    /// cascade) so a fresh attempt re-creates them from the top. Guarded on
    /// the run still being `leased`/`running`, so a cancel landing between
    /// selection and here wins (a canceled run is never requeued). Returns
    /// whether it acted.
    pub fn requeue_lost_run(&self, id: i64, next_attempt: i64) -> DbResult<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE runs SET status = 'queued', attempt = ?2, worker_id = NULL, \
             lease_expires_at = NULL, last_seq = 0, started_at = NULL, error = NULL, \
             finished_at = NULL \
             WHERE id = ?1 AND status IN ('leased', 'running')",
            params![id, next_attempt],
        )?;
        if n == 0 {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute("DELETE FROM task_runs WHERE run_id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
    }

    /// In-flight run count per worker: `leased`/`running` runs grouped by
    /// `worker_id`. Feeds the worker-status panel's load meter.
    pub fn in_flight_by_worker(&self) -> DbResult<HashMap<String, u64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT worker_id, COUNT(*) FROM runs \
             WHERE status IN ('leased', 'running') AND worker_id IS NOT NULL \
             GROUP BY worker_id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Record that update sequence `seq` has been applied to `run_id`, but
    /// only if it strictly exceeds the highest already applied. Returns `true`
    /// when accepted (the caller should apply the update), `false` when `seq`
    /// is stale/duplicate (a replay to be ignored).
    pub fn bump_seq(&self, run_id: i64, seq: i64) -> DbResult<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE runs SET last_seq = ?2 WHERE id = ?1 AND last_seq < ?2",
            params![run_id, seq],
        )?;
        Ok(n > 0)
    }

    // -- task_runs ------------------------------------------------------------

    /// Insert or update the task_run row for `(run_id, task_id)` with the
    /// given status and attempt. `started_at` is stamped the first time the
    /// status is `running` and preserved afterwards. Returns the task_run id.
    pub fn upsert_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        status: &str,
        attempt: i64,
    ) -> DbResult<i64> {
        let conn = self.conn()?;
        let now = now_rfc3339();
        let started_at = (status == "running").then(|| now.clone());
        // `created_at` is set on the first insert and never overwritten (it is
        // absent from the ON CONFLICT SET clause, so it is preserved on update).
        conn.execute(
            "INSERT INTO task_runs (run_id, task_id, status, attempt, created_at, started_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (run_id, task_id) DO UPDATE SET \
               status = excluded.status, attempt = excluded.attempt, \
               started_at = COALESCE(task_runs.started_at, excluded.started_at)",
            params![run_id, task_id, status, attempt, now, started_at],
        )?;
        let id = conn.query_row(
            "SELECT id FROM task_runs WHERE run_id = ?1 AND task_id = ?2",
            params![run_id, task_id],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Finalize a task_run: terminal status, result/outputs/error, and
    /// `finished_at = now`. See [`TaskRunFinish`].
    pub fn finish_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        finish: TaskRunFinish<'_>,
    ) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE task_runs SET status = ?3, result = ?4, outputs = ?5, error = ?6, \
             finished_at = ?7 WHERE run_id = ?1 AND task_id = ?2",
            params![
                run_id,
                task_id,
                finish.status,
                finish.result,
                finish.outputs,
                finish.error,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// The `task_runs.id` for `(run_id, task_id)`, if the row exists. Used by
    /// the worker-update applier to resolve a stable task name to the local
    /// autoincrement id its items hang off.
    pub fn find_task_run_id(&self, run_id: i64, task_id: &str) -> DbResult<Option<i64>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT id FROM task_runs WHERE run_id = ?1 AND task_id = ?2",
                params![run_id, task_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Task runs for a run, in insertion (execution) order.
    pub fn list_task_runs(&self, run_id: i64) -> DbResult<Vec<TaskRunRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, task_id, status, attempt, result, outputs, error, \
             created_at, started_at, finished_at FROM task_runs WHERE run_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([run_id], map_task_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- items ----------------------------------------------------------------

    /// Bulk-insert fan-out items (status `queued`, attempt 0) in one
    /// transaction; `idx` follows slice order starting at 0.
    pub fn insert_items(&self, task_run_id: i64, items: &[serde_json::Value]) -> DbResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO task_run_items (task_run_id, idx, item, status, attempt) \
                 VALUES (?1, ?2, ?3, 'queued', 0)",
            )?;
            for (idx, item) in items.iter().enumerate() {
                stmt.execute(params![task_run_id, idx as i64, item.to_string()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Update one item by `(task_run_id, idx)`. See [`ItemUpdate`] for which
    /// fields preserve existing values when `None`.
    pub fn update_item(&self, task_run_id: i64, idx: i64, update: ItemUpdate<'_>) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE task_run_items SET status = ?3, attempt = ?4, result = ?5, error = ?6, \
             started_at = COALESCE(?7, started_at), finished_at = COALESCE(?8, finished_at) \
             WHERE task_run_id = ?1 AND idx = ?2",
            params![
                task_run_id,
                idx,
                update.status,
                update.attempt,
                update.result,
                update.error,
                update.started_at,
                update.finished_at
            ],
        )?;
        Ok(())
    }

    /// List items in idx order with an optional status filter. `page` is
    /// 1-based. Returns the page of rows and the total matching count.
    pub fn list_items(
        &self,
        task_run_id: i64,
        status: Option<&str>,
        page: u32,
        per: u32,
    ) -> DbResult<(Vec<ItemRow>, u64)> {
        let conn = self.conn()?;
        let mut where_sql = String::from("task_run_id = ?");
        let mut filters: Vec<&dyn ToSql> = vec![&task_run_id];
        if let Some(s) = status.as_ref() {
            where_sql.push_str(" AND status = ?");
            filters.push(s);
        }

        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM task_run_items WHERE {where_sql}"),
            &filters[..],
            |r| r.get(0),
        )?;

        let limit = i64::from(per);
        let offset = i64::from(page.max(1) - 1) * limit;
        let mut params_vec = filters;
        params_vec.push(&limit);
        params_vec.push(&offset);
        let mut stmt = conn.prepare(&format!(
            "SELECT id, task_run_id, idx, item, status, attempt, result, error, \
             started_at, finished_at FROM task_run_items WHERE {where_sql} \
             ORDER BY idx LIMIT ? OFFSET ?"
        ))?;
        let rows = stmt
            .query_map(&params_vec[..], map_item)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total as u64))
    }

    /// Per-status counts for a fan-out task, plus `retried` (attempt > 1).
    pub fn item_aggregates(&self, task_run_id: i64) -> DbResult<ItemAggregates> {
        let conn = self.conn()?;
        let agg = conn.query_row(
            "SELECT COUNT(*), \
               IFNULL(SUM(status = 'queued'), 0), \
               IFNULL(SUM(status = 'running'), 0), \
               IFNULL(SUM(status = 'success'), 0), \
               IFNULL(SUM(status = 'failed'), 0), \
               IFNULL(SUM(status = 'dropped'), 0), \
               IFNULL(SUM(attempt > 1), 0) \
             FROM task_run_items WHERE task_run_id = ?1",
            [task_run_id],
            |r| {
                Ok(ItemAggregates {
                    total: r.get::<_, i64>(0)? as u64,
                    queued: r.get::<_, i64>(1)? as u64,
                    running: r.get::<_, i64>(2)? as u64,
                    success: r.get::<_, i64>(3)? as u64,
                    failed: r.get::<_, i64>(4)? as u64,
                    dropped: r.get::<_, i64>(5)? as u64,
                    retried: r.get::<_, i64>(6)? as u64,
                })
            },
        )?;
        Ok(agg)
    }

    /// One character per item in idx order:
    /// q(ueued) r(unning) s(uccess) f(ailed) d(ropped) c(anceled).
    pub fn item_statuses_compact(&self, task_run_id: i64) -> DbResult<String> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT status FROM task_run_items WHERE task_run_id = ?1 ORDER BY idx")?;
        let mut out = String::new();
        let rows = stmt.query_map([task_run_id], |r| r.get::<_, String>(0))?;
        for status in rows {
            out.push(match status?.as_str() {
                "queued" => 'q',
                "running" => 'r',
                "success" => 's',
                "failed" => 'f',
                "dropped" => 'd',
                "canceled" => 'c',
                _ => '?',
            });
        }
        Ok(out)
    }

    // -- logs -----------------------------------------------------------------

    /// Append a log line (`ts = now`). Returns the log id.
    pub fn append_log(&self, run_id: i64, level: &str, task: &str, message: &str) -> DbResult<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO logs (run_id, ts, level, task, message) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, now_rfc3339(), level, task, message],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Logs for a run with `id > after_id`, ascending, at most `limit` rows.
    pub fn list_logs(&self, run_id: i64, after_id: i64, limit: u32) -> DbResult<Vec<LogRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, ts, level, task, message FROM logs \
             WHERE run_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![run_id, after_id, i64::from(limit)], |r| {
                Ok(LogRow {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    ts: r.get(2)?,
                    level: r.get(3)?,
                    task: r.get(4)?,
                    message: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- schedule_state ---------------------------------------------------------

    /// Sync schedule_state with a flow's current triggers: insert missing
    /// rows (enabled, with the given `next_fire_at`), delete rows whose
    /// trigger no longer exists, and leave surviving rows untouched here
    /// (`enabled`, `last_fired_at`, and `next_fire_at` preserved). The
    /// scheduler's `reconcile_definition` then overwrites each survivor's
    /// `enabled` from the definition and may recompute `next_fire_at`.
    pub fn reconcile_schedules(
        &self,
        flow_id: &str,
        triggers: &[(&str, Option<&str>)],
    ) -> DbResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let existing: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT trigger_id FROM schedule_state WHERE flow_id = ?1")?;
            stmt.query_map([flow_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for stale in existing
            .iter()
            .filter(|e| !triggers.iter().any(|(t, _)| t == e))
        {
            tx.execute(
                "DELETE FROM schedule_state WHERE flow_id = ?1 AND trigger_id = ?2",
                params![flow_id, stale],
            )?;
        }
        for (trigger_id, next_fire_at) in triggers {
            if !existing.iter().any(|e| e == trigger_id) {
                tx.execute(
                    "INSERT INTO schedule_state (flow_id, trigger_id, enabled, next_fire_at) \
                     VALUES (?1, ?2, 1, ?3)",
                    params![flow_id, trigger_id, next_fire_at],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All schedule rows joined with their flow's name and paused flag.
    pub fn list_schedules(&self) -> DbResult<Vec<ScheduleRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT s.flow_id, f.name, f.paused, s.trigger_id, s.enabled, \
             s.next_fire_at, s.last_fired_at \
             FROM schedule_state s JOIN flows f ON f.id = s.flow_id \
             ORDER BY f.name, s.trigger_id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ScheduleRow {
                    flow_id: r.get(0)?,
                    flow_name: r.get(1)?,
                    flow_paused: r.get(2)?,
                    trigger_id: r.get(3)?,
                    enabled: r.get(4)?,
                    next_fire_at: r.get(5)?,
                    last_fired_at: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_schedule_enabled(
        &self,
        flow_id: &str,
        trigger_id: &str,
        enabled: bool,
    ) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE schedule_state SET enabled = ?3 WHERE flow_id = ?1 AND trigger_id = ?2",
            params![flow_id, trigger_id, enabled],
        )?;
        Ok(())
    }

    pub fn update_schedule_fired(
        &self,
        flow_id: &str,
        trigger_id: &str,
        last_fired_at: &str,
        next_fire_at: Option<&str>,
    ) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE schedule_state SET last_fired_at = ?3, next_fire_at = ?4 \
             WHERE flow_id = ?1 AND trigger_id = ?2",
            params![flow_id, trigger_id, last_fired_at, next_fire_at],
        )?;
        Ok(())
    }

    pub fn set_schedule_next(
        &self,
        flow_id: &str,
        trigger_id: &str,
        next_fire_at: Option<&str>,
    ) -> DbResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE schedule_state SET next_fire_at = ?3 \
             WHERE flow_id = ?1 AND trigger_id = ?2",
            params![flow_id, trigger_id, next_fire_at],
        )?;
        Ok(())
    }

    // -- recovery ----------------------------------------------------------------

    /// Ids of the `leased`/`running` runs a worker currently holds. Feeds the
    /// in-process worker's free-slot accounting and lease heartbeat.
    pub fn in_flight_run_ids(&self, worker_id: &str) -> DbResult<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM runs WHERE worker_id = ?1 AND status IN ('leased', 'running')",
        )?;
        let ids = stmt
            .query_map([worker_id], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Full rows of every `leased`/`running` run, regardless of lease expiry.
    /// Used at startup: after an unclean shutdown no worker has re-established
    /// anything, so all in-flight runs are lost and get resolved (requeued or
    /// failed) by the engine.
    pub fn all_in_flight_runs(&self) -> DbResult<Vec<RunRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, flow_id, flow_rev, status, trigger, inputs, queue, scheduled_for, \
             started_at, finished_at, error, worker_id, lease_expires_at, last_seq, attempt \
             FROM runs WHERE status IN ('leased', 'running') ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], map_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- dashboard ------------------------------------------------------------------

    /// Aggregate metrics for the dashboard. A run counts toward the 24h
    /// window when it started (or, lacking `started_at`, finished) within the
    /// last 24 hours, or has not started yet (queued). `next_scheduled` comes
    /// from [`Db::list_schedules`], not from here.
    pub fn dashboard_metrics(&self) -> DbResult<DashboardMetrics> {
        let conn = self.conn()?;
        let now = chrono::Utc::now();
        let cutoff_24h = (now - chrono::Duration::hours(24))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let cutoff_30d =
            (now - chrono::Duration::days(30)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let active_flows: i64 =
            conn.query_row("SELECT COUNT(*) FROM flows WHERE paused = 0", [], |r| {
                r.get(0)
            })?;

        let runs_24h = conn.query_row(
            "SELECT COUNT(*), \
               IFNULL(SUM(status = 'success'), 0), \
               IFNULL(SUM(status = 'degraded'), 0), \
               IFNULL(SUM(status = 'failed'), 0), \
               IFNULL(SUM(status = 'running'), 0) \
             FROM runs \
             WHERE COALESCE(started_at, finished_at) IS NULL \
                OR COALESCE(started_at, finished_at) >= ?1",
            [&cutoff_24h],
            |r| {
                Ok(Runs24h {
                    total: r.get::<_, i64>(0)? as u64,
                    ok: r.get::<_, i64>(1)? as u64,
                    degraded: r.get::<_, i64>(2)? as u64,
                    failed: r.get::<_, i64>(3)? as u64,
                    running: r.get::<_, i64>(4)? as u64,
                })
            },
        )?;

        // A `degraded` run finished but is not a clean success, so it counts
        // toward the denominator (dragging the rate down) without being a
        // success in the numerator.
        let (success_30d, degraded_30d, failed_30d, avg_duration_sec_30d): (
            i64,
            i64,
            i64,
            Option<f64>,
        ) = conn.query_row(
            "SELECT IFNULL(SUM(status = 'success'), 0), \
               IFNULL(SUM(status = 'degraded'), 0), \
               IFNULL(SUM(status = 'failed'), 0), \
               AVG(CASE WHEN started_at IS NOT NULL \
                   THEN (julianday(finished_at) - julianday(started_at)) * 86400.0 END) \
             FROM runs WHERE status IN ('success', 'degraded', 'failed') \
               AND finished_at IS NOT NULL AND finished_at >= ?1",
            [&cutoff_30d],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let finished = success_30d + degraded_30d + failed_30d;
        let success_rate_30d = (finished > 0).then(|| success_30d as f64 / finished as f64);

        Ok(DashboardMetrics {
            active_flows: active_flows as u64,
            runs_24h,
            success_rate_30d,
            avg_duration_sec_30d,
        })
    }

    /// Per-flow run stats for every flow that has at least one run, batched
    /// in two queries (no per-flow round trips). Flows with no runs are
    /// absent from the map; use [`FlowRunStats::default`] for them.
    pub fn flow_run_stats(&self) -> DbResult<HashMap<String, FlowRunStats>> {
        let conn = self.conn()?;
        let cutoff_30d = (chrono::Utc::now() - chrono::Duration::days(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        // Most recent run (by id) per flow, any status.
        let mut stats: HashMap<String, FlowRunStats> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT r.flow_id, r.status, r.finished_at FROM runs r \
             JOIN (SELECT flow_id, MAX(id) AS max_id FROM runs GROUP BY flow_id) m \
               ON m.max_id = r.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (flow_id, status, finished_at) = row?;
            let entry = stats.entry(flow_id).or_default();
            entry.last_run_status = Some(status);
            entry.last_run_finished_at = finished_at;
        }

        // 30-day success rate and average duration per flow.
        let mut stmt = conn.prepare(
            "SELECT flow_id, \
               IFNULL(SUM(status = 'success'), 0), \
               IFNULL(SUM(status = 'degraded'), 0), \
               IFNULL(SUM(status = 'failed'), 0), \
               AVG(CASE WHEN started_at IS NOT NULL \
                   THEN (julianday(finished_at) - julianday(started_at)) * 86400.0 END) \
             FROM runs WHERE status IN ('success', 'degraded', 'failed') \
               AND finished_at IS NOT NULL AND finished_at >= ?1 \
             GROUP BY flow_id",
        )?;
        let rows = stmt.query_map([&cutoff_30d], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<f64>>(4)?,
            ))
        })?;
        for row in rows {
            // `degraded` runs finished but are not clean successes: counted in
            // the denominator, not the numerator (see `dashboard_metrics`).
            let (flow_id, success, degraded, failed, avg_duration) = row?;
            let entry = stats.entry(flow_id).or_default();
            let finished = success + degraded + failed;
            entry.success_rate_30d = (finished > 0).then(|| success as f64 / finished as f64);
            entry.avg_duration_sec_30d = avg_duration;
        }

        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// Migrations + row mappers
// ---------------------------------------------------------------------------

/// Apply any migrations not yet recorded in the `migrations` table, each in
/// its own transaction.
fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS migrations (\
           version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    for (version, sql) in MIGRATIONS {
        let applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM migrations WHERE version = ?1)",
            [version],
            |r| r.get(0),
        )?;
        if applied {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now_rfc3339()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn map_flow(r: &rusqlite::Row<'_>) -> rusqlite::Result<FlowRow> {
    Ok(FlowRow {
        id: r.get(0)?,
        name: r.get(1)?,
        namespace: r.get(2)?,
        description: r.get(3)?,
        definition: r.get(4)?,
        current_rev: r.get(5)?,
        paused: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn map_revision(r: &rusqlite::Row<'_>) -> rusqlite::Result<FlowRevisionRow> {
    Ok(FlowRevisionRow {
        flow_id: r.get(0)?,
        rev: r.get(1)?,
        definition: r.get(2)?,
        message: r.get(3)?,
        created_at: r.get(4)?,
    })
}

fn map_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        flow_id: r.get(1)?,
        flow_rev: r.get(2)?,
        status: r.get(3)?,
        trigger: r.get(4)?,
        inputs: r.get(5)?,
        queue: r.get(6)?,
        scheduled_for: r.get(7)?,
        started_at: r.get(8)?,
        finished_at: r.get(9)?,
        error: r.get(10)?,
        worker_id: r.get(11)?,
        lease_expires_at: r.get(12)?,
        last_seq: r.get(13)?,
        attempt: r.get(14)?,
    })
}

fn map_task_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRunRow> {
    Ok(TaskRunRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        task_id: r.get(2)?,
        status: r.get(3)?,
        attempt: r.get(4)?,
        result: r.get(5)?,
        outputs: r.get(6)?,
        error: r.get(7)?,
        created_at: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
    })
}

fn map_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        id: r.get(0)?,
        task_run_id: r.get(1)?,
        idx: r.get(2)?,
        item: r.get(3)?,
        status: r.get(4)?,
        attempt: r.get(5)?,
        result: r.get(6)?,
        error: r.get(7)?,
        started_at: r.get(8)?,
        finished_at: r.get(9)?,
    })
}
