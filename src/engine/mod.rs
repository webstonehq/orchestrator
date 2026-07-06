//! Run engine: creates runs, executes them (sequential tasks, parallel
//! fan-out, retries, timeouts, cancellation), and broadcasts live events.
//!
//! # Lifecycle
//!
//! 1. [`Engine::create_run`] validates the caller's inputs against the flow's
//!    *current* definition (typed checks, required inputs, rendered defaults)
//!    and inserts a `queued` run row.
//! 2. [`Engine::start`] spawns a Tokio task that executes the run and returns
//!    immediately. While the run is active it can be observed live via
//!    [`Engine::subscribe`] and stopped via [`Engine::cancel`].
//! 3. When the run reaches a terminal status (`success` / `failed` /
//!    `canceled`) it is dropped from the active set; the broadcast channel
//!    closes, which ends any SSE streams.
//!
//! # Execution semantics
//!
//! Tasks run sequentially in definition order. Per plugin task the engine
//! renders the config from the run context, executes the plugin under a
//! per-attempt timeout (default 60s), retries retryable errors per the task's
//! exponential-backoff policy, and extracts declared outputs into the context
//! (`outputs.<task>.<name>`). A `parallel` task fans its child chain out over
//! a rendered items array with bounded concurrency; see the design doc §5.
//!
//! # Redaction and secret late-binding
//!
//! Secrets are resolved once at run start. Every string the engine persists
//! or broadcasts — log messages, task results, extracted outputs, item
//! payloads/results, and all error messages — passes through
//! [`crate::expr::redact`] against the resolved secret *values*, so secret
//! material never reaches the database or an SSE client.
//!
//! Input values (provided or defaulted) that reference `secrets.*` are
//! stored on the run row as their **raw template strings** and only rendered
//! at execution time ("late binding"), so plaintext secrets never land in
//! `runs.inputs` either. The same run-start finalization pass applies
//! defaults to inputs missing from the stored object, which makes
//! scheduler-created runs (inserted with `{}` inputs) work without a
//! create-time resolution step.

mod events;
mod run;
pub mod sink;
pub mod wire;

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, DbError};
use crate::expr;
use crate::model::{FlowDefinition, InputType};
use crate::plugins::PluginRegistry;
use crate::secrets::{SecretStore, SecretsError};

pub use events::RunEvent;
pub use sink::{LocalSink, RunSink};
pub use wire::{Assignment, RemoteSink, RunUpdate, SeqUpdate, UpdateBatch, apply_update};

/// Boxed future returned by a [`Sleeper`].
pub type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Injectable sleep function used for retry backoff.
///
/// Production uses `tokio::time::sleep`; tests inject a recording no-op so
/// retry/backoff behavior is testable without wall-clock sleeps (see
/// [`Engine::new_with_sleeper`]).
pub type Sleeper = Box<dyn Fn(Duration) -> SleepFuture + Send + Sync>;

/// Errors returned by the engine's public API.
#[derive(Debug)]
pub enum EngineError {
    /// No flow with this id exists.
    UnknownFlow(String),
    /// No run with this id exists.
    UnknownRun(i64),
    /// Provided inputs failed validation; one human-readable message per
    /// problem. The API layer maps this to a 422.
    InvalidInput(Vec<String>),
    /// [`Engine::start`] was called for a run that is not in `queued` status
    /// (already started, finished, or currently active).
    NotQueued(i64),
    /// A stored flow definition failed to parse.
    BadDefinition(String),
    /// Database failure.
    Db(DbError),
    /// Secret store failure (e.g. undecryptable secrets).
    Secrets(SecretsError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::UnknownFlow(id) => write!(f, "unknown flow \"{id}\""),
            EngineError::UnknownRun(id) => write!(f, "unknown run {id}"),
            EngineError::InvalidInput(errors) => {
                write!(f, "invalid inputs: {}", errors.join("; "))
            }
            EngineError::NotQueued(id) => write!(f, "run {id} is not queued"),
            EngineError::BadDefinition(msg) => write!(f, "bad flow definition: {msg}"),
            EngineError::Db(e) => write!(f, "database error: {e}"),
            EngineError::Secrets(e) => write!(f, "secrets error: {e}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::Db(e) => Some(e),
            EngineError::Secrets(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DbError> for EngineError {
    fn from(e: DbError) -> Self {
        EngineError::Db(e)
    }
}

impl From<SecretsError> for EngineError {
    fn from(e: SecretsError) -> Self {
        EngineError::Secrets(e)
    }
}

/// Book-keeping for one active (spawned, not yet finished) run.
struct RunHandle {
    cancel: CancellationToken,
    events: broadcast::Sender<RunEvent>,
}

/// Removes a run from the engine's active map on drop — including during a
/// panic unwind of the run task — so a crashed run can never leave a stale
/// entry (which would keep its broadcast channel, and thus SSE streams,
/// open forever).
struct ActiveGuard {
    engine: Arc<Engine>,
    run_id: i64,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        // Never panic in drop (a second panic would abort): recover the map
        // even if the mutex was poisoned.
        let mut active = match self.engine.active.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.remove(&self.run_id);
    }
}

/// How recently (seconds) a worker must have been seen to count as `online`
/// — a few of the worker's ~3s poll cycles.
const WORKER_ONLINE_WITHIN_SECS: i64 = 15;

/// A worker unseen for this many seconds is pruned from the registry (it
/// stopped polling — crashed, killed, or disconnected).
const WORKER_STALE_AFTER_SECS: i64 = 90;

/// What the registry remembers about one worker, refreshed on every claim
/// (the reliable liveness beat — workers poll `claim` even when idle) and
/// heartbeat.
#[derive(Debug, Clone)]
struct WorkerInfo {
    queues: Vec<String>,
    capacity: u32,
    last_seen: chrono::DateTime<chrono::Utc>,
}

/// A worker's status for the `/api/workers` panel: registry facts plus a live
/// in-flight count read from the database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerStatus {
    pub worker_id: String,
    pub queues: Vec<String>,
    pub capacity: u32,
    /// Runs this worker currently holds (`leased`/`running`).
    pub in_flight: u64,
    /// RFC3339 timestamp of the worker's last claim/heartbeat.
    pub last_seen: String,
    /// Seen within [`WORKER_ONLINE_WITHIN_SECS`].
    pub online: bool,
}

/// The run executor. One instance per process, shared behind an [`Arc`].
pub struct Engine {
    pub(crate) db: Db,
    pub(crate) registry: Arc<PluginRegistry>,
    secrets: Arc<SecretStore>,
    active: Mutex<HashMap<i64, RunHandle>>,
    /// Connected-worker registry, keyed by worker id (see [`WorkerInfo`]).
    workers: Mutex<HashMap<String, WorkerInfo>>,
    pub(crate) sleeper: Sleeper,
}

impl Engine {
    /// Create an engine using the real `tokio::time::sleep` for retry
    /// backoff.
    pub fn new(db: Db, registry: Arc<PluginRegistry>, secrets: Arc<SecretStore>) -> Arc<Self> {
        Self::new_with_sleeper(
            db,
            registry,
            secrets,
            Box::new(|d| Box::pin(tokio::time::sleep(d))),
        )
    }

    /// Create an engine with an injected backoff [`Sleeper`].
    ///
    /// Test hook: inject a recording no-op sleeper so retry tests assert the
    /// exact backoff schedule without sleeping.
    pub fn new_with_sleeper(
        db: Db,
        registry: Arc<PluginRegistry>,
        secrets: Arc<SecretStore>,
        sleeper: Sleeper,
    ) -> Arc<Self> {
        Arc::new(Engine {
            db,
            registry,
            secrets,
            active: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            sleeper,
        })
    }

    /// Validate `inputs` against the flow's *current* definition and insert a
    /// `queued` run. Returns the new run id.
    ///
    /// Input resolution, per declared input:
    /// - A provided or defaulted **string that is a template referencing
    ///   `secrets.*`** is stored as the raw template string — no rendering,
    ///   no typed parse. It is late-bound (rendered, coerced, type-checked)
    ///   at run start, so plaintext secrets never reach `runs.inputs`.
    /// - Any other provided value must match the declared type
    ///   (`STRING`/`DATE` want a string, `INT` an integer, `BOOLEAN` a bool,
    ///   `ARRAY` an array, `JSON` anything). No coercion — a mismatch is an
    ///   error.
    /// - A missing input with a non-secret `default` gets the default
    ///   template rendered against `{ vars }` (vars are literal strings,
    ///   never template-rendered). If the target type is `ARRAY`/`JSON`/
    ///   `INT`/`BOOLEAN` and the rendered value is still a string, it is
    ///   parsed as JSON text (so an `ARRAY` default like `"[\"ON\"]"`
    ///   becomes a real array). The final value must match the declared
    ///   type.
    /// - A missing *required* input without a default is an error.
    /// - Provided keys that are not declared inputs are errors.
    ///
    /// All problems are collected into [`EngineError::InvalidInput`] (the API
    /// maps it to a 422). The resolved inputs object is stored on the run
    /// row; run start applies a finalization pass (missing defaults +
    /// secret-template rendering) without ever writing back to the row.
    pub fn create_run(
        &self,
        flow_id: &str,
        inputs: Map<String, Value>,
        trigger: &str,
        scheduled_for: Option<&str>,
    ) -> Result<i64, EngineError> {
        let flow = self
            .db
            .get_flow(flow_id)?
            .ok_or_else(|| EngineError::UnknownFlow(flow_id.to_string()))?;
        let def: FlowDefinition = serde_json::from_str(&flow.definition).map_err(|e| {
            EngineError::BadDefinition(format!(
                "flow \"{flow_id}\": invalid stored definition: {e}"
            ))
        })?;
        let resolved = self.resolve_inputs(&def, inputs)?;
        let inputs_json = Value::Object(resolved).to_string();
        Ok(self.db.insert_run(
            flow_id,
            flow.current_rev,
            trigger,
            &inputs_json,
            &def.queue,
            scheduled_for,
        )?)
    }

    /// Spawn the execution task for a `queued` run; returns immediately.
    ///
    /// Starting a run that is not queued (already started, finished, or
    /// currently active) fails with [`EngineError::NotQueued`], so calls are
    /// idempotent-safe: at most one execution per run ever starts.
    pub fn start(self: &Arc<Self>, run_id: i64) -> Result<(), EngineError> {
        let mut active = self.active.lock().expect("engine.active poisoned");
        if active.contains_key(&run_id) {
            return Err(EngineError::NotQueued(run_id));
        }
        let run = self
            .db
            .get_run(run_id)?
            .ok_or(EngineError::UnknownRun(run_id))?;
        if run.status != "queued" {
            return Err(EngineError::NotQueued(run_id));
        }
        // Routing: the in-process executor only serves the `local` queue. A
        // run targeting another queue is left `queued` for a worker
        // subscribed to that queue to claim; starting it here is a no-op.
        if run.queue != crate::model::LOCAL_QUEUE {
            drop(active);
            tracing::info!(
                run_id,
                queue = %run.queue,
                "run left queued for a `{}` worker",
                run.queue
            );
            return Ok(());
        }
        let cancel = CancellationToken::new();
        let (tx, _) = broadcast::channel(1024);
        active.insert(
            run_id,
            RunHandle {
                cancel: cancel.clone(),
                events: tx.clone(),
            },
        );
        drop(active);

        // The in-process sink persists to SQLite and broadcasts on the run's
        // live channel — the same channel `subscribe` hands to SSE clients.
        let sink: Arc<dyn sink::RunSink> =
            Arc::new(sink::LocalSink::new(self.db.clone(), tx.clone()));

        let engine = Arc::clone(self);
        tokio::spawn(async move {
            // Drop-guard: the active-map entry is removed (closing the
            // broadcast channel, which ends SSE streams) even if execution
            // panics.
            let guard = ActiveGuard { engine, run_id };
            run::execute_run(&guard.engine, run, cancel, sink).await;
        });
        Ok(())
    }

    /// Queued-run convenience for the scheduler/API:
    /// [`Engine::create_run`] followed by [`Engine::start`].
    pub fn create_and_start(
        self: &Arc<Self>,
        flow_id: &str,
        inputs: Map<String, Value>,
        trigger: &str,
        scheduled_for: Option<&str>,
    ) -> Result<i64, EngineError> {
        let run_id = self.create_run(flow_id, inputs, trigger, scheduled_for)?;
        self.start(run_id)?;
        Ok(run_id)
    }

    /// Execute an already-loaded run to completion, reporting through `sink`.
    ///
    /// The engine's own database supplies the flow definition (its revision
    /// row must exist) and, via the sink, holds the run's state. A worker
    /// calls this with a [`RemoteSink`](wire::RemoteSink) over a local scratch
    /// database to stream the run back to the control plane; the server uses
    /// [`Engine::start`] with a [`LocalSink`](sink::LocalSink) instead.
    pub async fn execute_to_sink(
        &self,
        run: crate::db::RunRow,
        token: CancellationToken,
        sink: Arc<dyn sink::RunSink>,
    ) {
        run::execute_run(self, run, token, sink).await;
    }

    // -- remote runs (workers) -----------------------------------------------

    /// Lease queued runs on `queues` to `worker_id`, up to its *total*
    /// `capacity` (the server computes free slots by subtracting the runs the
    /// worker already holds — the caller passes total capacity, not remaining
    /// slots). Returns the newly-claimed runs as [`Assignment`]s. Each claimed
    /// run is registered as active (so SSE clients can attach) with its own
    /// live channel and a cancellation token the worker learns about via
    /// heartbeats.
    pub fn claim_remote(
        self: &Arc<Self>,
        worker_id: &str,
        queues: &[&str],
        capacity: u32,
        lease_secs: i64,
    ) -> Result<Vec<wire::Assignment>, EngineError> {
        // A claim is the reliable liveness beat: workers poll it every cycle
        // even when idle, so this keeps queues/capacity/last-seen current.
        // `capacity` here is the worker's TOTAL configured capacity.
        self.workers.lock().expect("engine.workers poisoned").insert(
            worker_id.to_string(),
            WorkerInfo {
                queues: queues.iter().map(|q| q.to_string()).collect(),
                capacity,
                last_seen: chrono::Utc::now(),
            },
        );
        // Free slots = total capacity minus runs this worker already holds.
        let held = self
            .db
            .in_flight_by_worker()?
            .get(worker_id)
            .copied()
            .unwrap_or(0);
        let available = capacity.saturating_sub(u32::try_from(held).unwrap_or(u32::MAX));
        let rows = self.db.claim_runs(worker_id, queues, available, lease_secs)?;
        let mut assignments = Vec::with_capacity(rows.len());
        for run in rows {
            self.remote_channel(run.id);
            let definition = match self.db.get_revision(&run.flow_id, run.flow_rev)? {
                Some(rev) => rev.definition,
                None => match self.db.get_flow(&run.flow_id)? {
                    Some(flow) => flow.definition,
                    None => continue,
                },
            };
            assignments.push(wire::Assignment {
                run_id: run.id,
                flow_id: run.flow_id,
                flow_rev: run.flow_rev,
                trigger: run.trigger,
                queue: run.queue,
                inputs: run.inputs,
                definition,
            });
        }
        Ok(assignments)
    }

    /// Apply one worker-reported update to run `run_id`, deduplicating by
    /// `seq`. Returns `true` if applied, `false` if it was a stale/duplicate
    /// replay. A terminal run-status update ends the run's live channel.
    pub fn apply_remote_update(
        self: &Arc<Self>,
        run_id: i64,
        seq: i64,
        update: wire::RunUpdate,
    ) -> Result<bool, EngineError> {
        if !self.db.bump_seq(run_id, seq)? {
            return Ok(false);
        }
        let tx = self.remote_channel(run_id);
        let terminal = matches!(
            &update,
            wire::RunUpdate::RunStatus { status, .. }
                if matches!(status.as_str(), "success" | "failed" | "canceled")
        );
        wire::apply_update(&self.db, &tx, run_id, update)?;
        if terminal {
            self.active
                .lock()
                .expect("engine.active poisoned")
                .remove(&run_id);
        }
        Ok(true)
    }

    /// Renew `worker_id`'s leases and report back which of `run_ids` have been
    /// cancelled (so the worker can stop them). A run no longer active is
    /// reported cancelled too — it was reaped or finished server-side.
    pub fn heartbeat_remote(
        &self,
        worker_id: &str,
        run_ids: &[i64],
        lease_secs: i64,
    ) -> Result<Vec<i64>, EngineError> {
        // Refresh liveness for a known worker (heartbeats carry no queues/
        // capacity — those come from claims). An unknown id is left alone;
        // the worker's next claim registers it.
        if !worker_id.is_empty()
            && let Some(info) = self
                .workers
                .lock()
                .expect("engine.workers poisoned")
                .get_mut(worker_id)
        {
            info.last_seen = chrono::Utc::now();
        }
        self.db.renew_leases(worker_id, run_ids, lease_secs)?;
        let active = self.active.lock().expect("engine.active poisoned");
        let canceled = run_ids
            .iter()
            .copied()
            .filter(|id| match active.get(id) {
                Some(handle) => handle.cancel.is_cancelled(),
                None => true,
            })
            .collect();
        Ok(canceled)
    }

    /// Snapshot of connected workers for the status panel. Prunes entries
    /// unseen past [`WORKER_STALE_AFTER_SECS`] (dead workers drop off on their
    /// own), joins each with its live in-flight count from the database, and
    /// sorts by worker id.
    pub fn worker_statuses(&self) -> Result<Vec<WorkerStatus>, EngineError> {
        let now = chrono::Utc::now();
        let mut registry = self.workers.lock().expect("engine.workers poisoned");
        registry.retain(|_, info| {
            (now - info.last_seen).num_seconds() < WORKER_STALE_AFTER_SECS
        });
        let snapshot: Vec<(String, WorkerInfo)> =
            registry.iter().map(|(id, i)| (id.clone(), i.clone())).collect();
        drop(registry);

        let in_flight = self.db.in_flight_by_worker()?;
        let mut out: Vec<WorkerStatus> = snapshot
            .into_iter()
            .map(|(worker_id, info)| {
                let age = (now - info.last_seen).num_seconds();
                WorkerStatus {
                    in_flight: in_flight.get(&worker_id).copied().unwrap_or(0),
                    online: age < WORKER_ONLINE_WITHIN_SECS,
                    last_seen: info
                        .last_seen
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    queues: info.queues,
                    capacity: info.capacity,
                    worker_id,
                }
            })
            .collect();
        out.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
        Ok(out)
    }

    /// Reap runs whose worker lease lapsed: fail them in the database and end
    /// their live channels. Returns the reaped run ids.
    pub fn reap_lost_runs(&self) -> Result<Vec<i64>, EngineError> {
        let ids = self.db.reap_expired_leases(&crate::db::now_rfc3339())?;
        let mut active = self.active.lock().expect("engine.active poisoned");
        for id in &ids {
            if let Some(handle) = active.remove(id) {
                let _ = handle.events.send(RunEvent::Run {
                    status: "failed".to_string(),
                    finished_at: Some(crate::db::now_rfc3339()),
                    error: Some("worker lost (lease expired)".to_string()),
                });
            }
        }
        Ok(ids)
    }

    /// Get (creating if absent) the live broadcast channel for a remote run,
    /// registering it in the active map so `subscribe`/`cancel` work for it.
    fn remote_channel(self: &Arc<Self>, run_id: i64) -> broadcast::Sender<RunEvent> {
        let mut active = self.active.lock().expect("engine.active poisoned");
        active
            .entry(run_id)
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(1024);
                RunHandle {
                    cancel: CancellationToken::new(),
                    events: tx,
                }
            })
            .events
            .clone()
    }

    /// Request cancellation of an active run. Returns `true` if the run was
    /// active (the cancellation token was flipped); `false` otherwise.
    ///
    /// Cancellation is cooperative and prompt: the in-flight plugin call is
    /// raced against the token, queued fan-out items are marked `canceled`
    /// without ever starting, and the run finishes with status `canceled`.
    pub fn cancel(&self, run_id: i64) -> bool {
        match self
            .active
            .lock()
            .expect("engine.active poisoned")
            .get(&run_id)
        {
            Some(handle) => {
                handle.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Subscribe to a run's live [`RunEvent`] stream.
    ///
    /// Returns `None` when the run is not active (never started or already
    /// finished) — the caller falls back to a database snapshot. The channel
    /// closes when the run finishes.
    pub fn subscribe(&self, run_id: i64) -> Option<broadcast::Receiver<RunEvent>> {
        self.active
            .lock()
            .expect("engine.active poisoned")
            .get(&run_id)
            .map(|handle| handle.events.subscribe())
    }

    /// Startup recovery after an unclean shutdown: marks runs/tasks that were
    /// still `queued`/`running` as failed ("interrupted by shutdown") and
    /// their items `canceled`. Returns the number of rows changed.
    pub fn recover_interrupted(&self) -> Result<u64, EngineError> {
        Ok(self.db.mark_interrupted()?)
    }

    /// Number of currently executing runs (for the UI's engine widget).
    pub fn active_run_count(&self) -> usize {
        self.active.lock().expect("engine.active poisoned").len()
    }

    /// See [`Engine::create_run`] for the resolution rules.
    fn resolve_inputs(
        &self,
        def: &FlowDefinition,
        mut provided: Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        let mut errors = Vec::new();
        let mut out = Map::new();
        // Context for rendering non-secret defaults; built at most once.
        // Deliberately contains NO secrets: secret-referencing values are
        // stored raw and late-bound at run start.
        let mut default_ctx: Option<Value> = None;

        for input in &def.inputs {
            if let Some(value) = provided.remove(&input.id) {
                // Secret-referencing template: store the raw template string,
                // skip the typed check (both happen at run start).
                if let Value::String(s) = &value
                    && references_secrets(s)
                {
                    out.insert(input.id.clone(), value);
                    continue;
                }
                if type_matches(input.input_type, &value) {
                    out.insert(input.id.clone(), value);
                } else {
                    errors.push(format!(
                        "input \"{}\": expected {}, got {}",
                        input.id,
                        type_label(input.input_type),
                        json_type_name(&value)
                    ));
                }
                continue;
            }
            if let Some(default) = &input.default {
                // Secret-referencing default: store the raw template string;
                // it is rendered with the full context at run start.
                if references_secrets(default) {
                    out.insert(input.id.clone(), Value::String(default.clone()));
                    continue;
                }
                let ctx = match &default_ctx {
                    Some(ctx) => ctx,
                    None => default_ctx.insert(create_time_render_ctx(def)),
                };
                match expr::render(default, ctx) {
                    Ok(rendered) => match parse_default(input.input_type, rendered) {
                        Ok(value) => {
                            out.insert(input.id.clone(), value);
                        }
                        Err(msg) => errors.push(format!("input \"{}\": {msg}", input.id)),
                    },
                    Err(e) => {
                        errors.push(format!(
                            "input \"{}\": default template error: {e}",
                            input.id
                        ));
                    }
                }
                continue;
            }
            if input.required {
                errors.push(format!("input \"{}\" is required", input.id));
            }
        }

        for key in provided.keys() {
            errors.push(format!("unknown input \"{key}\""));
        }

        if errors.is_empty() {
            Ok(out)
        } else {
            Err(EngineError::InvalidInput(errors))
        }
    }

    /// Resolve all secrets for run execution (values map, for the context and
    /// for redaction).
    pub(crate) fn resolve_secrets(&self) -> Result<HashMap<String, String>, SecretsError> {
        self.secrets.resolve_all()
    }
}

/// Context for rendering non-secret input defaults at create time: variables
/// as literal strings (never template-rendered). Contains no secrets — any
/// secret-referencing value is stored raw and rendered at run start instead.
fn create_time_render_ctx(def: &FlowDefinition) -> Value {
    let vars: Map<String, Value> = def
        .variables
        .iter()
        .map(|v| (v.id.clone(), Value::String(v.value.clone())))
        .collect();
    json!({ "vars": vars })
}

/// Whether a string parses as a template with at least one reference rooted
/// at `secrets`. Strings that fail to parse are not templates (`false`).
pub(crate) fn references_secrets(template: &str) -> bool {
    expr::referenced_paths(template).is_ok_and(|paths| {
        paths
            .iter()
            .any(|p| p == "secrets" || p.starts_with("secrets.") || p.starts_with("secrets["))
    })
}

/// Finalize a rendered input value (default or late-bound secret template):
/// string-encoded structured types are parsed as JSON text, then the value
/// must match the declared type.
pub(crate) fn parse_default(input_type: InputType, rendered: Value) -> Result<Value, String> {
    let value = match (&rendered, input_type) {
        (
            Value::String(s),
            InputType::Array | InputType::Json | InputType::Int | InputType::Boolean,
        ) => serde_json::from_str::<Value>(s)
            .map_err(|_| format!("value {s:?} is not valid {}", type_label(input_type)))?,
        _ => rendered,
    };
    if type_matches(input_type, &value) {
        Ok(value)
    } else {
        Err(format!(
            "value rendered to {}, expected {}",
            json_type_name(&value),
            type_label(input_type)
        ))
    }
}

/// Typed check for input values (no coercion).
fn type_matches(input_type: InputType, value: &Value) -> bool {
    match input_type {
        InputType::String | InputType::Date => value.is_string(),
        InputType::Int => value.is_i64() || value.is_u64(),
        InputType::Boolean => value.is_boolean(),
        InputType::Array => value.is_array(),
        InputType::Json => true,
    }
}

fn type_label(input_type: InputType) -> &'static str {
    match input_type {
        InputType::String => "STRING",
        InputType::Array => "ARRAY",
        InputType::Date => "DATE",
        InputType::Int => "INT",
        InputType::Boolean => "BOOLEAN",
        InputType::Json => "JSON",
    }
}

pub(crate) fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
