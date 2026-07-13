//! Run routes: list/detail, cancel, replay, logs, fan-out items, SSE.
//!
//! # SSE protocol (`GET /api/runs/:id/events`)
//!
//! 1. On connect the server immediately sends event `snapshot` whose data is
//!    the same JSON as `GET /api/runs/:id` (results omitted) plus
//!    `last_log_id` — the highest log id already written for the run, so the
//!    client knows where to start paging `/logs`.
//! 2. If the run is active, the engine's broadcast channel is bridged: each
//!    [`RunEvent`] becomes an SSE event named [`RunEvent::event_name`]
//!    (`run` / `task` / `items` / `log`) with the serialized event as data.
//!    A lagged receiver gets a fresh `snapshot` and continues.
//! 3. When the broadcast closes (run finished) — or immediately after the
//!    snapshot when the run was not active — the server sends event `end`
//!    and closes the stream. Keep-alive comments go out every 15 seconds.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use futures::Stream;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::db::{Db, FlowRow, ItemRow, RunRow, TaskRunRow};
use crate::engine::{EngineError, RunEvent};
use crate::model::{FlowDefinition, TaskKind, ValidationErr};

use super::{ApiError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(run_detail))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/replay", post(replay_run))
        .route("/runs/{id}/logs", get(run_logs))
        .route("/runs/{id}/events", get(run_events))
        .route("/runs/{id}/tasks/{task}/items", get(task_items))
        .route("/runs/{id}/tasks/{task}/retry-failed", post(retry_failed))
}

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// One run in the list/detail responses. Superset of the DB row: `inputs` is
/// parsed JSON, `duration_sec` is derived from the timestamps, and
/// `tasks_done` / `tasks_total` summarize task progress.
#[derive(Serialize)]
struct RunSummary {
    id: i64,
    flow_id: String,
    flow_rev: i64,
    status: String,
    trigger: String,
    queue: String,
    inputs: Value,
    scheduled_for: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    error: Option<String>,
    /// Wall-clock seconds between `started_at` and `finished_at`; only
    /// present when both timestamps exist.
    duration_sec: Option<f64>,
    /// Task runs in a terminal state (success/failed/canceled/skipped).
    tasks_done: u64,
    /// Top-level task count in the run's definition revision (0 when the
    /// revision is missing or unparsable).
    tasks_total: u64,
}

/// One task run in the detail response. `result` can be large, so it is
/// omitted unless the client asks with `?include_result=true`.
#[derive(Serialize)]
struct TaskRunView {
    id: i64,
    run_id: i64,
    task_id: String,
    status: String,
    attempt: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    outputs: Option<Value>,
    error: Option<String>,
    created_at: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

/// One fan-out item in the items response, JSON columns parsed.
#[derive(Serialize)]
struct ItemView {
    id: i64,
    idx: i64,
    item: Value,
    status: String,
    attempt: i64,
    result: Value,
    error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

/// Parse a JSON text column, falling back to `null` on bad data rather than
/// failing the whole response.
fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn duration_sec(started_at: Option<&str>, finished_at: Option<&str>) -> Option<f64> {
    let start = chrono::DateTime::parse_from_rfc3339(started_at?).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(finished_at?).ok()?;
    Some((end - start).num_milliseconds() as f64 / 1000.0)
}

fn is_terminal_task(status: &str) -> bool {
    matches!(status, "success" | "failed" | "canceled" | "skipped")
}

/// Load and parse the flow definition revision a run executed against.
/// `None` when the revision row is gone or its JSON no longer parses.
fn load_definition(db: &Db, flow_id: &str, rev: i64) -> Result<Option<FlowDefinition>, ApiError> {
    let Some(row) = db.get_revision(flow_id, rev)? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&row.definition).ok())
}

fn run_summary(run: &RunRow, task_rows: &[TaskRunRow], def: Option<&FlowDefinition>) -> RunSummary {
    RunSummary {
        id: run.id,
        flow_id: run.flow_id.clone(),
        flow_rev: run.flow_rev,
        status: run.status.clone(),
        trigger: run.trigger.clone(),
        queue: run.queue.clone(),
        inputs: parse_json(&run.inputs),
        scheduled_for: run.scheduled_for.clone(),
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        error: run.error.clone(),
        duration_sec: duration_sec(run.started_at.as_deref(), run.finished_at.as_deref()),
        tasks_done: task_rows
            .iter()
            .filter(|t| is_terminal_task(&t.status))
            .count() as u64,
        tasks_total: def.map(|d| d.tasks.len() as u64).unwrap_or(0),
    }
}

/// 422 body in the canonical `{"errors": [{path, message}]}` shape; the
/// convention source is `flows.rs::validation_response`.
fn validation_errors_response(errors: Vec<ValidationErr>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "errors": errors })),
    )
        .into_response()
}

/// Create-and-start a run, mapping [`EngineError::InvalidInput`] to the same
/// errors-array 422 that `flows.rs` uses (one entry per message, path
/// `inputs`).
///
/// Takes the flow row so every launch path (replay, retry-failed) hits the
/// same pause gate as `POST /flows/:id/run`: a paused flow is a 409.
fn launch_run(state: &AppState, flow: &FlowRow, inputs: Map<String, Value>) -> Response {
    if flow.paused {
        return ApiError::conflict("flow is paused").into_response();
    }
    match state.engine.create_run(&flow.id, inputs, "manual", None) {
        Ok(run_id) => Json(json!({ "run_id": run_id })).into_response(),
        Err(EngineError::InvalidInput(messages)) => {
            let errors: Vec<ValidationErr> = messages
                .into_iter()
                .map(|message| ValidationErr {
                    path: "inputs".to_string(),
                    message,
                })
                .collect();
            validation_errors_response(errors)
        }
        Err(EngineError::UnknownFlow(id)) => {
            ApiError::not_found(format!("flow `{id}`")).into_response()
        }
        Err(e) => ApiError::internal(e).into_response(),
    }
}

fn run_not_found(id: i64) -> ApiError {
    ApiError::not_found(format!("run {id}"))
}

fn get_run_or_404(db: &Db, id: i64) -> Result<RunRow, ApiError> {
    db.get_run(id)?.ok_or_else(|| run_not_found(id))
}

/// Empty query-string values (`?flow=&status=`) mean "no filter".
fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// GET /runs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    flow: Option<String>,
    status: Option<String>,
    trigger: Option<String>,
    /// Inclusive lower bound on `started_at` (RFC3339).
    since: Option<String>,
    /// Exclusive upper bound on `started_at` (RFC3339).
    until: Option<String>,
    page: Option<u32>,
    per: Option<u32>,
}

async fn list_runs(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let page = q.page.unwrap_or(1).max(1);
    let per = q.per.unwrap_or(25).min(200);
    let flow = non_empty(q.flow);
    let status = non_empty(q.status);
    let trigger = non_empty(q.trigger);
    let since = non_empty(q.since);
    let until = non_empty(q.until);
    let (rows, total) = state.db.list_runs(
        flow.as_deref(),
        status.as_deref(),
        trigger.as_deref(),
        since.as_deref(),
        until.as_deref(),
        page,
        per,
    )?;

    // Cache parsed definitions per (flow, rev) across the page.
    let mut defs: HashMap<(String, i64), Option<FlowDefinition>> = HashMap::new();
    let mut runs = Vec::with_capacity(rows.len());
    for run in &rows {
        let key = (run.flow_id.clone(), run.flow_rev);
        let def = match defs.get(&key) {
            Some(def) => def,
            None => {
                let loaded = load_definition(&state.db, &run.flow_id, run.flow_rev)?;
                defs.entry(key).or_insert(loaded)
            }
        };
        let task_rows = state.db.list_task_runs(run.id)?;
        runs.push(run_summary(run, &task_rows, def.as_ref()));
    }

    let by_status = state.db.count_runs_by_status()?;
    let count = |s: &str| by_status.get(s).copied().unwrap_or(0);
    let counts = json!({
        "all": by_status.values().sum::<u64>(),
        "running": count("running"),
        "success": count("success"),
        "degraded": count("degraded"),
        "failed": count("failed"),
        "queued": count("queued"),
        "canceled": count("canceled"),
    });

    Ok(Json(
        json!({ "runs": runs, "total": total, "counts": counts }),
    ))
}

// ---------------------------------------------------------------------------
// GET /runs/:id
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DetailQuery {
    #[serde(default)]
    include_result: bool,
}

/// The `GET /runs/:id` payload; also the body of the SSE `snapshot` event.
fn build_run_detail(state: &AppState, id: i64, include_result: bool) -> Result<Value, ApiError> {
    let run = get_run_or_404(&state.db, id)?;
    let def = load_definition(&state.db, &run.flow_id, run.flow_rev)?;
    let task_rows = state.db.list_task_runs(id)?;

    let tasks: Vec<TaskRunView> = task_rows
        .iter()
        .map(|t| TaskRunView {
            id: t.id,
            run_id: t.run_id,
            task_id: t.task_id.clone(),
            status: t.status.clone(),
            attempt: t.attempt,
            result: include_result
                .then(|| t.result.as_deref().map(parse_json).unwrap_or(Value::Null)),
            outputs: t.outputs.as_deref().map(parse_json),
            error: t.error.clone(),
            created_at: t.created_at.clone(),
            started_at: t.started_at.clone(),
            finished_at: t.finished_at.clone(),
        })
        .collect();

    // Fan-out aggregates for every top-level parallel task in the run's
    // definition revision; zeros when the task has not started yet.
    let mut fanout = Map::new();
    if let Some(def) = &def {
        for task in &def.tasks {
            if matches!(task.kind, TaskKind::Parallel(_)) {
                let agg = match task_rows.iter().find(|t| t.task_id == task.id) {
                    Some(t) => state.db.item_aggregates(t.id)?,
                    None => Default::default(),
                };
                fanout.insert(
                    task.id.clone(),
                    serde_json::to_value(agg).unwrap_or_default(),
                );
            }
        }
    }

    Ok(json!({
        "run": run_summary(&run, &task_rows, def.as_ref()),
        "tasks": tasks,
        "fanout": fanout,
    }))
}

async fn run_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<DetailQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(build_run_detail(&state, id, q.include_result)?))
}

// ---------------------------------------------------------------------------
// POST /runs/:id/cancel — POST /runs/:id/replay
// ---------------------------------------------------------------------------

async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    get_run_or_404(&state.db, id)?;
    let canceled = state.engine.cancel(id);
    Ok(Json(json!({ "canceled": canceled })))
}

async fn replay_run(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let run = get_run_or_404(&state.db, id)?;
    let inputs = match parse_json(&run.inputs) {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let flow = state
        .db
        .get_flow(&run.flow_id)?
        .ok_or_else(|| ApiError::not_found(format!("flow `{}`", run.flow_id)))?;
    // Replays validate against the flow's *current* definition; inputs that
    // no longer fit surface as an errors-array 422.
    Ok(launch_run(&state, &flow, inputs))
}

// ---------------------------------------------------------------------------
// GET /runs/:id/logs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LogsQuery {
    after_id: Option<i64>,
    limit: Option<u32>,
}

async fn run_logs(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, ApiError> {
    get_run_or_404(&state.db, id)?;
    let limit = q.limit.unwrap_or(500).min(2000);
    let logs: Vec<Value> = state
        .db
        .list_logs(id, q.after_id.unwrap_or(0), limit)?
        .into_iter()
        .map(|l| {
            json!({
                "id": l.id, "ts": l.ts, "level": l.level,
                "task": l.task, "message": l.message,
            })
        })
        .collect();
    Ok(Json(json!({ "logs": logs })))
}

// ---------------------------------------------------------------------------
// GET /runs/:id/tasks/:task/items
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ItemsQuery {
    status: Option<String>,
    page: Option<u32>,
    per: Option<u32>,
    format: Option<String>,
}

/// Find the task_run row for `(run_id, task_id)`, 404ing on unknown run or
/// task.
fn get_task_run_or_404(db: &Db, run_id: i64, task_id: &str) -> Result<TaskRunRow, ApiError> {
    get_run_or_404(db, run_id)?;
    db.list_task_runs(run_id)?
        .into_iter()
        .find(|t| t.task_id == task_id)
        .ok_or_else(|| ApiError::not_found(format!("task `{task_id}` in run {run_id}")))
}

fn item_view(row: ItemRow) -> ItemView {
    ItemView {
        id: row.id,
        idx: row.idx,
        item: parse_json(&row.item),
        status: row.status,
        attempt: row.attempt,
        result: row.result.as_deref().map(parse_json).unwrap_or(Value::Null),
        error: row.error,
        started_at: row.started_at,
        finished_at: row.finished_at,
    }
}

async fn task_items(
    State(state): State<AppState>,
    Path((id, task)): Path<(i64, String)>,
    Query(q): Query<ItemsQuery>,
) -> Result<Json<Value>, ApiError> {
    let task_run = get_task_run_or_404(&state.db, id, &task)?;

    if q.format.as_deref() == Some("heatmap") {
        let statuses = state.db.item_statuses_compact(task_run.id)?;
        let total = statuses.chars().count();
        return Ok(Json(json!({ "statuses": statuses, "total": total })));
    }

    let page = q.page.unwrap_or(1).max(1);
    let per = q.per.unwrap_or(50).min(500);
    let status = non_empty(q.status);
    let (rows, total) = state
        .db
        .list_items(task_run.id, status.as_deref(), page, per)?;
    let items: Vec<ItemView> = rows.into_iter().map(item_view).collect();
    Ok(Json(json!({ "items": items, "total": total })))
}

// ---------------------------------------------------------------------------
// POST /runs/:id/tasks/:task/retry-failed
// ---------------------------------------------------------------------------

async fn retry_failed(
    State(state): State<AppState>,
    Path((id, task)): Path<(i64, String)>,
) -> Result<Response, ApiError> {
    let run = get_run_or_404(&state.db, id)?;
    let task_run = get_task_run_or_404(&state.db, id, &task)?;

    // Collect every failed item's original value, paging internally.
    let mut values = Vec::new();
    let mut page = 1;
    loop {
        let (rows, total) = state
            .db
            .list_items(task_run.id, Some("failed"), page, 500)?;
        values.extend(rows.iter().map(|r| parse_json(&r.item)));
        if values.len() as u64 >= total || rows.is_empty() {
            break;
        }
        page += 1;
    }
    if values.is_empty() {
        return Err(ApiError::bad_request(format!(
            "task `{task}` in run {id} has no failed items to retry"
        )));
    }

    // Documented convention: the new run receives the failed item values as
    // the `items` input — the flow must declare one.
    let flow = state
        .db
        .get_flow(&run.flow_id)?
        .ok_or_else(|| ApiError::not_found(format!("flow `{}`", run.flow_id)))?;
    let declares_items = serde_json::from_str::<FlowDefinition>(&flow.definition)
        .map(|def| def.inputs.iter().any(|i| i.id == "items"))
        .unwrap_or(false);
    if !declares_items {
        return Ok(validation_errors_response(vec![ValidationErr {
            path: "inputs".to_string(),
            message: format!(
                "flow `{}` has no `items` input to receive retried items — \
                 add one or re-run manually",
                run.flow_id
            ),
        }]));
    }

    let mut inputs = Map::new();
    inputs.insert("items".to_string(), Value::Array(values));
    Ok(launch_run(&state, &flow, inputs))
}

// ---------------------------------------------------------------------------
// GET /runs/:id/events (SSE)
// ---------------------------------------------------------------------------

/// Max log id already written for a run; the SSE snapshot carries it so
/// clients know where `/logs?after_id=` paging starts.
fn last_log_id(db: &Db, run_id: i64) -> Result<i64, ApiError> {
    let conn = db.conn()?;
    conn.query_row(
        "SELECT IFNULL(MAX(id), 0) FROM logs WHERE run_id = ?1",
        [run_id],
        |r| r.get(0),
    )
    .map_err(ApiError::internal)
}

fn snapshot_event(state: &AppState, run_id: i64) -> Result<Event, ApiError> {
    let mut detail = build_run_detail(state, run_id, false)?;
    detail["last_log_id"] = json!(last_log_id(&state.db, run_id)?);
    Ok(Event::default().event("snapshot").data(detail.to_string()))
}

fn end_event() -> Event {
    Event::default().event("end").data("{}")
}

/// What the SSE stream does after the initial snapshot.
enum SseStep {
    /// Bridge the engine's broadcast channel.
    Live(broadcast::Receiver<RunEvent>),
    /// Send `end`, then close.
    End,
    /// Close the stream.
    Closed,
}

async fn run_events(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    get_run_or_404(&state.db, id)?;
    // Subscribe before snapshotting so no event between the two is lost
    // (duplicated state in the snapshot is harmless).
    let step = match state.engine.subscribe(id) {
        Some(rx) => SseStep::Live(rx),
        None => SseStep::End,
    };
    let snapshot = snapshot_event(&state, id)?;

    let live = stream::unfold((state, id, step), |(state, id, step)| async move {
        match step {
            SseStep::Live(mut rx) => loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                        let event = Event::default().event(ev.event_name()).data(data);
                        return Some((Ok(event), (state, id, SseStep::Live(rx))));
                    }
                    // Fell behind the broadcast: resync with a fresh snapshot.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        match snapshot_event(&state, id) {
                            Ok(event) => {
                                return Some((Ok(event), (state, id, SseStep::Live(rx))));
                            }
                            Err(_) => continue,
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Some((Ok(end_event()), (state, id, SseStep::Closed)));
                    }
                }
            },
            SseStep::End => Some((Ok(end_event()), (state, id, SseStep::Closed))),
            SseStep::Closed => None,
        }
    });

    let stream = stream::once(async move { Ok(snapshot) }).chain(live);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
