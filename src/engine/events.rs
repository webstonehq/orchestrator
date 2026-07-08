//! Live run events broadcast to SSE subscribers.
//!
//! While a run is active the engine broadcasts [`RunEvent`]s on a
//! `tokio::sync::broadcast` channel obtained via
//! [`crate::engine::Engine::subscribe`]. The API layer bridges the channel to
//! `GET /api/runs/:id/events`: each variant serializes to the JSON payload of
//! one SSE message, and [`RunEvent::event_name`] supplies the SSE event name
//! (`run` / `task` / `items` / `log`).
//!
//! The channel closes (receivers see `RecvError::Closed`) when the run
//! finishes and the engine drops its sender — that is the SSE stream's
//! natural end.

use serde::Serialize;

use crate::db::ItemAggregates;

/// One live-update event from an executing run.
///
/// Serializes *untagged*: the JSON is just the variant's fields, matching the
/// SSE data payloads in the API contract. Use [`RunEvent::event_name`] for
/// the SSE event name.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum RunEvent {
    /// The run changed status (`running`, `success`, `degraded`, `failed`,
    /// `canceled`).
    Run {
        /// New run status.
        status: String,
        /// RFC3339 finish time; present only on terminal statuses.
        #[serde(skip_serializing_if = "Option::is_none")]
        finished_at: Option<String>,
        /// Run error message (secret-redacted); present only on failure.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A task changed status (`running`, `success`, `failed`, `canceled`,
    /// `skipped`).
    Task {
        /// The task's id within the flow.
        task_id: String,
        /// New task status.
        status: String,
        /// Attempt number (1-based; 0 for statuses without an attempt, e.g.
        /// `skipped`).
        attempt: u32,
    },
    /// Fan-out progress for a parallel task. Throttled to at most one event
    /// per 500ms per task, plus a final event when the fan-out completes.
    Items {
        /// The parallel task's id.
        task_id: String,
        /// Per-status item counts (flattened into the JSON payload).
        #[serde(flatten)]
        agg: ItemAggregates,
        /// Completed items (success + failed + dropped) per second since the
        /// fan-out started.
        throughput_per_sec: f64,
    },
    /// A log line was appended (message already secret-redacted).
    Log {
        /// The `logs` row id (usable as `after_id` for catch-up queries).
        id: i64,
        /// RFC3339 timestamp.
        ts: String,
        /// `INFO` | `OK` | `WARN` | `ERR` | `DBG`.
        level: String,
        /// Task id the line belongs to, or `flow` for run-level lines.
        task: String,
        /// Redacted log message.
        message: String,
    },
}

impl RunEvent {
    /// The SSE event name for this variant (`run`, `task`, `items`, `log`).
    pub fn event_name(&self) -> &'static str {
        match self {
            RunEvent::Run { .. } => "run",
            RunEvent::Task { .. } => "task",
            RunEvent::Items { .. } => "items",
            RunEvent::Log { .. } => "log",
        }
    }
}
