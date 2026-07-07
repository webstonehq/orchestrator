//! The run-update wire protocol: how a worker's execution reports back.
//!
//! A worker runs the real engine loop against a local *scratch* database
//! through a [`RemoteSink`]. Every [`RunSink`] call is (a) forwarded to that
//! local scratch — so `upsert_task_run` returns a real id, `item_aggregates`
//! reads back, and live behaviour is unchanged — and (b) translated into a
//! [`RunUpdate`] pushed onto a channel bound for the server. The server feeds
//! each arriving update through [`apply_update`], which performs the exact
//! same SQLite writes and broadcasts a local run would, so a remote run and a
//! local run converge on identical state.
//!
//! Two design points keep the two databases consistent despite having
//! independent autoincrement ids:
//!
//! - Updates identify a task by its **stable `task_id`**, never by the
//!   worker's local `task_runs.id`; the server resolves the name to its own
//!   row id ([`Db::find_task_run_id`]).
//! - **Log ids are the server's.** The worker's `emit(RunEvent::Log)` is
//!   dropped on the wire; the server re-derives the live `log` event from the
//!   [`RunUpdate::AppendLog`] it persists, using the id its own insert
//!   assigned — so SSE `after_id` paging stays coherent.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

use crate::db::{
    Db, DbResult, ItemAggregates, ItemUpdate, RunStatusUpdate, TaskRunFinish, now_rfc3339,
};

use super::RunEvent;
use super::sink::{LocalSink, RunSink};

/// One reported state transition of a remotely-executing run. Mirrors the
/// [`RunSink`] operations, but keyed by stable `task_id`/`idx` rather than
/// local autoincrement ids so the server can apply it against its own rows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunUpdate {
    /// Persist a run-status transition (`runs` row).
    RunStatus {
        status: String,
        error: Option<String>,
        started_at: Option<String>,
        finished_at: Option<String>,
    },
    /// Insert-or-update a `task_runs` row.
    TaskUpsert {
        task_id: String,
        status: String,
        attempt: i64,
    },
    /// Finalize a `task_runs` row.
    TaskFinish {
        task_id: String,
        status: String,
        result: Option<String>,
        outputs: Option<String>,
        error: Option<String>,
    },
    /// Bulk-insert a parallel task's fan-out items.
    ItemsInsert { task_id: String, items: Vec<Value> },
    /// Update one fan-out item by index.
    ItemUpdate {
        task_id: String,
        idx: i64,
        status: String,
        attempt: i64,
        result: Option<String>,
        error: Option<String>,
        started_at: Option<String>,
        finished_at: Option<String>,
    },
    /// Append a (redacted) log line. The server assigns the id.
    AppendLog {
        level: String,
        task: String,
        message: String,
    },
    /// Live-only: a run-status event for SSE subscribers.
    EventRun {
        status: String,
        finished_at: Option<String>,
        error: Option<String>,
    },
    /// Live-only: a task-status event for SSE subscribers.
    EventTask {
        task_id: String,
        status: String,
        attempt: u32,
    },
    /// Live-only: throttled fan-out progress for SSE subscribers.
    EventItems {
        task_id: String,
        agg: ItemAggregates,
        throughput_per_sec: f64,
    },
}

/// A run handed to a worker to execute: everything its local engine needs to
/// run the flow without reaching back for more (the definition and inputs
/// travel; secrets do not — the worker resolves its own).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Assignment {
    /// The server's run id; every reported update is tagged with it.
    pub run_id: i64,
    pub flow_id: String,
    pub flow_rev: i64,
    pub trigger: String,
    pub queue: String,
    /// The run's stored inputs (JSON object text).
    pub inputs: String,
    /// The flow definition JSON the run executes against.
    pub definition: String,
}

/// A worker's report of one run's updates since its last, tagged with the
/// server run id and per-run monotonic `seq`s for idempotent apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateBatch {
    pub run_id: i64,
    pub updates: Vec<SeqUpdate>,
}

/// One sequenced update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeqUpdate {
    pub seq: i64,
    pub update: RunUpdate,
}

/// A [`RunSink`] that tees to a local scratch [`LocalSink`] and emits a
/// [`RunUpdate`] for each operation to `updates`. See the module docs.
pub struct RemoteSink {
    /// Local scratch DB + broadcast, giving real ids and readbacks.
    inner: LocalSink,
    /// Outbound update channel (drained by the worker's connection loop).
    updates: mpsc::UnboundedSender<RunUpdate>,
    /// Local `task_runs.id` -> stable `task_id`, learned from upserts, so
    /// item operations (which arrive with a local id) can name their task.
    task_names: Mutex<HashMap<i64, String>>,
}

impl RemoteSink {
    pub fn new(inner: LocalSink, updates: mpsc::UnboundedSender<RunUpdate>) -> Self {
        RemoteSink {
            inner,
            updates,
            task_names: Mutex::new(HashMap::new()),
        }
    }

    fn send(&self, update: RunUpdate) {
        // A closed receiver means the connection dropped; execution continues
        // best-effort (the run will be reaped server-side if it never lands).
        let _ = self.updates.send(update);
    }

    fn task_name(&self, task_run_id: i64) -> Option<String> {
        self.task_names
            .lock()
            .expect("task_names poisoned")
            .get(&task_run_id)
            .cloned()
    }
}

impl RunSink for RemoteSink {
    fn update_run_status(&self, run_id: i64, update: RunStatusUpdate<'_>) -> DbResult<()> {
        self.send(RunUpdate::RunStatus {
            status: update.status.to_string(),
            error: update.error.map(str::to_string),
            started_at: update.started_at.map(str::to_string),
            finished_at: update.finished_at.map(str::to_string),
        });
        self.inner.update_run_status(run_id, update)
    }

    fn upsert_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        status: &str,
        attempt: i64,
    ) -> DbResult<i64> {
        let id = self
            .inner
            .upsert_task_run(run_id, task_id, status, attempt)?;
        self.task_names
            .lock()
            .expect("task_names poisoned")
            .insert(id, task_id.to_string());
        self.send(RunUpdate::TaskUpsert {
            task_id: task_id.to_string(),
            status: status.to_string(),
            attempt,
        });
        Ok(id)
    }

    fn finish_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        finish: TaskRunFinish<'_>,
    ) -> DbResult<()> {
        self.send(RunUpdate::TaskFinish {
            task_id: task_id.to_string(),
            status: finish.status.to_string(),
            result: finish.result.map(str::to_string),
            outputs: finish.outputs.map(str::to_string),
            error: finish.error.map(str::to_string),
        });
        self.inner.finish_task_run(run_id, task_id, finish)
    }

    fn insert_items(&self, task_run_id: i64, items: &[Value]) -> DbResult<()> {
        if let Some(task_id) = self.task_name(task_run_id) {
            self.send(RunUpdate::ItemsInsert {
                task_id,
                items: items.to_vec(),
            });
        }
        self.inner.insert_items(task_run_id, items)
    }

    fn update_item(&self, task_run_id: i64, idx: i64, update: ItemUpdate<'_>) -> DbResult<()> {
        if let Some(task_id) = self.task_name(task_run_id) {
            self.send(RunUpdate::ItemUpdate {
                task_id,
                idx,
                status: update.status.to_string(),
                attempt: update.attempt,
                result: update.result.map(str::to_string),
                error: update.error.map(str::to_string),
                started_at: update.started_at.map(str::to_string),
                finished_at: update.finished_at.map(str::to_string),
            });
        }
        self.inner.update_item(task_run_id, idx, update)
    }

    fn item_aggregates(&self, task_run_id: i64) -> DbResult<ItemAggregates> {
        // Pure read of the local scratch; not reported.
        self.inner.item_aggregates(task_run_id)
    }

    fn append_log(&self, run_id: i64, level: &str, task: &str, message: &str) -> DbResult<i64> {
        self.send(RunUpdate::AppendLog {
            level: level.to_string(),
            task: task.to_string(),
            message: message.to_string(),
        });
        self.inner.append_log(run_id, level, task, message)
    }

    fn emit(&self, event: RunEvent) {
        // Live events are re-derived server-side. `Log` is dropped on the
        // wire: the server re-emits it from `AppendLog` with its own id so
        // SSE `after_id` paging stays coherent.
        match &event {
            RunEvent::Run {
                status,
                finished_at,
                error,
            } => self.send(RunUpdate::EventRun {
                status: status.clone(),
                finished_at: finished_at.clone(),
                error: error.clone(),
            }),
            RunEvent::Task {
                task_id,
                status,
                attempt,
            } => self.send(RunUpdate::EventTask {
                task_id: task_id.clone(),
                status: status.clone(),
                attempt: *attempt,
            }),
            RunEvent::Items {
                task_id,
                agg,
                throughput_per_sec,
            } => self.send(RunUpdate::EventItems {
                task_id: task_id.clone(),
                agg: *agg,
                throughput_per_sec: *throughput_per_sec,
            }),
            RunEvent::Log { .. } => {}
        }
        self.inner.emit(event);
    }
}

/// Apply one worker-reported [`RunUpdate`] against the server's database and
/// live broadcast channel — the inverse of [`RemoteSink`]. Persistence writes
/// use the same [`Db`] methods a local run does; live events are broadcast on
/// `tx`. Task ids are resolved to local `task_runs.id`s via
/// [`Db::find_task_run_id`].
pub fn apply_update(
    db: &Db,
    tx: &broadcast::Sender<RunEvent>,
    run_id: i64,
    update: RunUpdate,
) -> DbResult<()> {
    match update {
        RunUpdate::RunStatus {
            status,
            error,
            started_at,
            finished_at,
        } => db.update_run_status(
            run_id,
            RunStatusUpdate {
                status: &status,
                error: error.as_deref(),
                started_at: started_at.as_deref(),
                finished_at: finished_at.as_deref(),
            },
        )?,
        RunUpdate::TaskUpsert {
            task_id,
            status,
            attempt,
        } => {
            db.upsert_task_run(run_id, &task_id, &status, attempt)?;
        }
        RunUpdate::TaskFinish {
            task_id,
            status,
            result,
            outputs,
            error,
        } => db.finish_task_run(
            run_id,
            &task_id,
            TaskRunFinish {
                status: &status,
                result: result.as_deref(),
                outputs: outputs.as_deref(),
                error: error.as_deref(),
            },
        )?,
        RunUpdate::ItemsInsert { task_id, items } => {
            if let Some(tri) = db.find_task_run_id(run_id, &task_id)? {
                db.insert_items(tri, &items)?;
            }
        }
        RunUpdate::ItemUpdate {
            task_id,
            idx,
            status,
            attempt,
            result,
            error,
            started_at,
            finished_at,
        } => {
            if let Some(tri) = db.find_task_run_id(run_id, &task_id)? {
                db.update_item(
                    tri,
                    idx,
                    ItemUpdate {
                        status: &status,
                        attempt,
                        result: result.as_deref(),
                        error: error.as_deref(),
                        started_at: started_at.as_deref(),
                        finished_at: finished_at.as_deref(),
                    },
                )?;
            }
        }
        RunUpdate::AppendLog {
            level,
            task,
            message,
        } => {
            let id = db.append_log(run_id, &level, &task, &message)?;
            let _ = tx.send(RunEvent::Log {
                id,
                ts: now_rfc3339(),
                level,
                task,
                message,
            });
        }
        RunUpdate::EventRun {
            status,
            finished_at,
            error,
        } => {
            let _ = tx.send(RunEvent::Run {
                status,
                finished_at,
                error,
            });
        }
        RunUpdate::EventTask {
            task_id,
            status,
            attempt,
        } => {
            let _ = tx.send(RunEvent::Task {
                task_id,
                status,
                attempt,
            });
        }
        RunUpdate::EventItems {
            task_id,
            agg,
            throughput_per_sec,
        } => {
            let _ = tx.send(RunEvent::Items {
                task_id,
                agg,
                throughput_per_sec,
            });
        }
    }
    Ok(())
}
