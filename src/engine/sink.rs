//! The [`RunSink`]: the seam between run execution and where its state lands.
//!
//! [`super::run::execute_run`] performs two kinds of side effect at every
//! step — it *persists* state (task/item rows, results, logs, run status) and
//! it *broadcasts* a live [`RunEvent`]. Both go through a `RunSink` rather
//! than touching the database or the broadcast channel directly, so the exact
//! same execution loop can run in two places:
//!
//! - On the **server**, a [`LocalSink`] forwards each call straight to the
//!   SQLite [`Db`] and the run's `tokio::broadcast` channel — behaviourally
//!   identical to the original inline code.
//! - On a **worker**, a remote sink (see `crate::worker`) serializes each call
//!   into a `RunUpdate` message and ships it to the server, which applies it
//!   through a `LocalSink` on the far side. The worker's engine thus reports
//!   run state back over the wire with no local database.
//!
//! The trait mirrors the subset of [`Db`] methods the run loop needs, keeping
//! their exact signatures (so the server path is a pure forward), plus
//! [`RunSink::emit`] for live events.

use serde_json::Value;
use tokio::sync::broadcast;

use crate::db::{Db, DbResult, ItemAggregates, ItemUpdate, RunStatusUpdate, TaskRunFinish};

use super::RunEvent;

/// Where an executing run's persisted state and live events are sent.
///
/// Every method mirrors a [`Db`] operation the run loop performs (identical
/// signature), plus [`RunSink::emit`] for the live broadcast. Persistence
/// methods return [`DbResult`] so the loop's existing log-and-continue error
/// handling is unchanged.
pub trait RunSink: Send + Sync {
    fn update_run_status(&self, run_id: i64, update: RunStatusUpdate<'_>) -> DbResult<()>;
    fn upsert_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        status: &str,
        attempt: i64,
    ) -> DbResult<i64>;
    fn finish_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        finish: TaskRunFinish<'_>,
    ) -> DbResult<()>;
    fn insert_items(&self, task_run_id: i64, items: &[Value]) -> DbResult<()>;
    fn update_item(&self, task_run_id: i64, idx: i64, update: ItemUpdate<'_>) -> DbResult<()>;
    fn item_aggregates(&self, task_run_id: i64) -> DbResult<ItemAggregates>;
    fn append_log(&self, run_id: i64, level: &str, task: &str, message: &str) -> DbResult<i64>;
    /// Broadcast a live event (best-effort; dropped if there are no
    /// subscribers).
    fn emit(&self, event: RunEvent);
}

/// The server-side sink: persists to SQLite and broadcasts to the run's live
/// channel. A pure forward to [`Db`] + the `tokio::broadcast` sender, so
/// in-process runs behave exactly as before the sink seam existed.
pub struct LocalSink {
    db: Db,
    tx: broadcast::Sender<RunEvent>,
}

impl LocalSink {
    pub fn new(db: Db, tx: broadcast::Sender<RunEvent>) -> Self {
        LocalSink { db, tx }
    }
}

impl RunSink for LocalSink {
    fn update_run_status(&self, run_id: i64, update: RunStatusUpdate<'_>) -> DbResult<()> {
        self.db.update_run_status(run_id, update)
    }
    fn upsert_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        status: &str,
        attempt: i64,
    ) -> DbResult<i64> {
        self.db.upsert_task_run(run_id, task_id, status, attempt)
    }
    fn finish_task_run(
        &self,
        run_id: i64,
        task_id: &str,
        finish: TaskRunFinish<'_>,
    ) -> DbResult<()> {
        self.db.finish_task_run(run_id, task_id, finish)
    }
    fn insert_items(&self, task_run_id: i64, items: &[Value]) -> DbResult<()> {
        self.db.insert_items(task_run_id, items)
    }
    fn update_item(&self, task_run_id: i64, idx: i64, update: ItemUpdate<'_>) -> DbResult<()> {
        self.db.update_item(task_run_id, idx, update)
    }
    fn item_aggregates(&self, task_run_id: i64) -> DbResult<ItemAggregates> {
        self.db.item_aggregates(task_run_id)
    }
    fn append_log(&self, run_id: i64, level: &str, task: &str, message: &str) -> DbResult<i64> {
        self.db.append_log(run_id, level, task, message)
    }
    fn emit(&self, event: RunEvent) {
        let _ = self.tx.send(event);
    }
}
