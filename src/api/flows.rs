//! Flow routes: list/CRUD, validation, revisions, pause, YAML
//! import/export, and manual run triggering.
//!
//! Validation failures (definition or run inputs) respond `422` with
//! `{"errors": [{"path", "message"}]}`; all other errors use the shared
//! [`ApiError`] shape `{"error": "..."}`.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::db::FlowRow;
use crate::engine::EngineError;
use crate::model::{self, FlowDefinition, ValidationErr};

use super::{ApiError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/flows", get(list_flows).post(create_flow))
        .route("/flows/validate", post(validate_flow))
        .route("/flows/import", post(import_flow))
        .route(
            "/flows/{id}",
            get(get_flow).put(update_flow).delete(delete_flow),
        )
        .route("/flows/{id}/pause", post(pause_flow))
        .route("/flows/{id}/revisions", get(list_revisions))
        .route("/flows/{id}/revisions/{rev}", get(get_revision))
        .route("/flows/{id}/export", get(export_flow))
        .route("/flows/{id}/run", post(run_flow))
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateFlowBody {
    #[serde(default)]
    id: Option<String>,
    definition: Value,
}

#[derive(Deserialize)]
struct UpdateFlowBody {
    definition: Value,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct PauseBody {
    paused: bool,
}

#[derive(Deserialize)]
struct ValidateBody {
    definition: Value,
}

#[derive(Deserialize)]
struct RunBody {
    #[serde(default)]
    inputs: Map<String, Value>,
    #[serde(default)]
    trigger: Option<String>,
}

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// One row of `GET /flows` (matches `FlowSummary` in `ui/src/lib/api.ts`).
#[derive(Serialize)]
struct FlowSummary {
    id: String,
    name: String,
    namespace: String,
    paused: bool,
    schedule_human: String,
    last_run: Option<LastRun>,
    success_rate_30d: Option<f64>,
    avg_duration_sec: Option<f64>,
    current_rev: i64,
}

#[derive(Serialize)]
struct LastRun {
    status: String,
    finished_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_flows(State(state): State<AppState>) -> Result<Json<Vec<FlowSummary>>, ApiError> {
    let stats = state.db.flow_run_stats()?;
    let mut out = Vec::new();
    for flow in state.db.list_flows()? {
        let s = stats.get(&flow.id).cloned().unwrap_or_default();
        // Best-effort: an unparseable stored definition (should not happen)
        // degrades to "manual" rather than failing the whole list.
        let schedule_human = serde_json::from_str::<FlowDefinition>(&flow.definition)
            .map(|def| schedule_human(&def))
            .unwrap_or_else(|_| "manual".to_string());
        out.push(FlowSummary {
            id: flow.id,
            name: flow.name,
            namespace: flow.namespace,
            paused: flow.paused,
            schedule_human,
            last_run: s.last_run_status.map(|status| LastRun {
                status,
                finished_at: s.last_run_finished_at,
            }),
            success_rate_30d: s.success_rate_30d,
            avg_duration_sec: s.avg_duration_sec_30d,
            current_rev: flow.current_rev,
        });
    }
    Ok(Json(out))
}

async fn create_flow(
    State(state): State<AppState>,
    Json(body): Json<CreateFlowBody>,
) -> Result<Response, ApiError> {
    let def = match parse_and_validate(body.definition, &state) {
        Ok(def) => def,
        Err(errors) => return Ok(validation_response(&errors)),
    };
    let id = match body.id {
        Some(id) => {
            if !model::is_valid_id(&id) {
                return Err(ApiError::bad_request(format!(
                    "invalid flow id `{id}`: must match [a-z][a-z0-9_]* (max 64 chars)"
                )));
            }
            id
        }
        None => slugify_flow_id(&def.name).ok_or_else(|| {
            ApiError::bad_request(format!("cannot derive a flow id from name {:?}", def.name))
        })?,
    };
    // Exists-check then upsert is a TOCTOU race; accepted for v1 (single
    // node, and the loser merely records a revision instead of a conflict).
    if state.db.get_flow(&id)?.is_some() {
        return Err(ApiError::conflict(format!("flow `{id}` already exists")));
    }
    save_and_reconcile(&state, &id, &def, "create")?;
    let row = fetch_flow(&state, &id)?;
    Ok((StatusCode::CREATED, Json(detail_json(&row)?)).into_response())
}

async fn get_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = fetch_flow(&state, &id)?;
    Ok(Json(detail_json(&row)?))
}

async fn update_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateFlowBody>,
) -> Result<Response, ApiError> {
    fetch_flow(&state, &id)?;
    let def = match parse_and_validate(body.definition, &state) {
        Ok(def) => def,
        Err(errors) => return Ok(validation_response(&errors)),
    };
    let message = body.message.as_deref().unwrap_or("update");
    let rev = save_and_reconcile(&state, &id, &def, message)?;
    Ok(Json(json!({ "current_rev": rev })).into_response())
}

async fn delete_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !state.db.delete_flow(&id)? {
        return Err(ApiError::not_found(format!("flow `{id}`")));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn pause_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PauseBody>,
) -> Result<StatusCode, ApiError> {
    fetch_flow(&state, &id)?;
    state.db.set_paused(&id, body.paused)?;
    if !body.paused {
        // Unpausing recomputes next fire times; the scheduler advanced them
        // silently while paused, so this is a freshness nicety, not a replay.
        reconcile_and_notify(&state, &id)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_revisions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    fetch_flow(&state, &id)?;
    let revisions: Vec<Value> = state
        .db
        .list_revisions(&id)?
        .into_iter()
        .map(|r| json!({ "rev": r.rev, "message": r.message, "created_at": r.created_at }))
        .collect();
    Ok(Json(Value::Array(revisions)))
}

async fn get_revision(
    State(state): State<AppState>,
    Path((id, rev)): Path<(String, i64)>,
) -> Result<Json<Value>, ApiError> {
    let row = state
        .db
        .get_revision(&id, rev)?
        .ok_or_else(|| ApiError::not_found(format!("revision {rev} of flow `{id}`")))?;
    let definition: Value = serde_json::from_str(&row.definition).map_err(ApiError::internal)?;
    Ok(Json(json!({ "definition": definition })))
}

async fn validate_flow(
    State(state): State<AppState>,
    Json(body): Json<ValidateBody>,
) -> Json<Value> {
    // Always 200; problems live in the body so the UI builder can render
    // them inline without special-casing an error status.
    let errors = match parse_and_validate(body.definition, &state) {
        Ok(_) => Vec::new(),
        Err(errors) => errors,
    };
    Json(json!({ "errors": errors }))
}

async fn export_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let row = fetch_flow(&state, &id)?;
    let def: FlowDefinition = serde_json::from_str(&row.definition).map_err(ApiError::internal)?;
    let yaml = model::to_yaml(&row.id, &def).map_err(ApiError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/yaml; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.yaml\"", row.id),
            ),
        ],
        yaml,
    )
        .into_response())
}

async fn import_flow(State(state): State<AppState>, body: String) -> Result<Response, ApiError> {
    let (id, def) = model::from_yaml(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if !model::is_valid_id(&id) {
        return Err(ApiError::bad_request(format!(
            "invalid flow id `{id}`: must match [a-z][a-z0-9_]* (max 64 chars)"
        )));
    }
    let errors = model::validate(&def, &state.registry);
    if !errors.is_empty() {
        return Ok(validation_response(&errors));
    }
    let existed = state.db.get_flow(&id)?.is_some();
    save_and_reconcile(&state, &id, &def, "import")?;
    let row = fetch_flow(&state, &id)?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(detail_json(&row)?)).into_response())
}

async fn run_flow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RunBody>,
) -> Result<Response, ApiError> {
    let flow = fetch_flow(&state, &id)?;
    if flow.paused {
        return Err(ApiError::conflict("flow is paused"));
    }
    // Clients may only claim `manual` or `api`; other trigger values
    // (`schedule`, ...) are reserved for the server's own launch paths.
    let trigger = body.trigger.as_deref().unwrap_or("manual");
    if !matches!(trigger, "manual" | "api") {
        return Err(ApiError::bad_request(format!(
            "invalid trigger `{trigger}`: must be `manual` or `api`"
        )));
    }
    match state
        .engine
        .create_and_start(&id, body.inputs, trigger, None)
    {
        Ok(run_id) => Ok(Json(json!({ "run_id": run_id })).into_response()),
        Err(EngineError::InvalidInput(messages)) => {
            let errors: Vec<ValidationErr> = messages
                .into_iter()
                .map(|message| ValidationErr {
                    path: "inputs".to_string(),
                    message,
                })
                .collect();
            Ok(validation_response(&errors))
        }
        Err(EngineError::UnknownFlow(_)) => Err(ApiError::not_found(format!("flow `{id}`"))),
        Err(e) => Err(ApiError::internal(e)),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load a flow row or 404.
fn fetch_flow(state: &AppState, id: &str) -> Result<FlowRow, ApiError> {
    state
        .db
        .get_flow(id)?
        .ok_or_else(|| ApiError::not_found(format!("flow `{id}`")))
}

/// Flow detail body: `{id, definition (parsed), current_rev, paused,
/// updated_at}`.
fn detail_json(row: &FlowRow) -> Result<Value, ApiError> {
    let definition: Value = serde_json::from_str(&row.definition).map_err(ApiError::internal)?;
    Ok(json!({
        "id": row.id,
        "definition": definition,
        "current_rev": row.current_rev,
        "paused": row.paused,
        "updated_at": row.updated_at,
    }))
}

/// The canonical `422 {"errors": [{path, message}]}` response.
fn validation_response(errors: &[ValidationErr]) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "errors": errors })),
    )
        .into_response()
}

/// Deserialize + validate a definition; `Err` is the issue list for a 422.
fn parse_and_validate(
    definition: Value,
    state: &AppState,
) -> Result<FlowDefinition, Vec<ValidationErr>> {
    let def: FlowDefinition = serde_json::from_value(definition).map_err(|e| {
        vec![ValidationErr {
            path: "definition".to_string(),
            message: e.to_string(),
        }]
    })?;
    let errors = model::validate(&def, &state.registry);
    if errors.is_empty() {
        Ok(def)
    } else {
        Err(errors)
    }
}

/// Persist a definition as a new revision, then resync schedule state and
/// poke the scheduler loop. Returns the new revision number.
fn save_and_reconcile(
    state: &AppState,
    id: &str,
    def: &FlowDefinition,
    message: &str,
) -> Result<i64, ApiError> {
    let definition_json = serde_json::to_string(def).map_err(ApiError::internal)?;
    let rev = state.db.upsert_flow_with_revision(
        id,
        &def.name,
        &def.namespace,
        &def.description,
        &definition_json,
        message,
    )?;
    reconcile_and_notify(state, id)?;
    Ok(rev)
}

fn reconcile_and_notify(state: &AppState, id: &str) -> Result<(), ApiError> {
    // Validation already vetted cron/timezone, so failures here are internal.
    state
        .scheduler
        .reconcile_flow(id)
        .map_err(ApiError::internal)?;
    state.scheduler.notify_handle().notify_one();
    Ok(())
}

/// Derive a flow id from a display name: lowercase, keep `[a-z0-9]`,
/// collapse every other run of characters to a single `_`, drop leading
/// characters until an ASCII letter, trim trailing `_`, cap at 64 chars.
/// `None` when nothing usable remains.
fn slugify_flow_id(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let mut out = String::new();
    for c in lower.chars() {
        if c.is_ascii_lowercase() || (!out.is_empty() && c.is_ascii_digit()) {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out.truncate(64);
    if model::is_valid_id(&out) {
        Some(out)
    } else {
        None
    }
}

/// Human label for a flow's schedule: the first *enabled* schedule trigger
/// humanized, or `"manual"` when there is none.
pub(crate) fn schedule_human(def: &FlowDefinition) -> String {
    def.triggers
        .iter()
        .find(|t| t.trigger_type == "schedule" && t.enabled)
        .map(|t| humanize_cron(&t.cron))
        .unwrap_or_else(|| "manual".to_string())
}

/// Best-effort humanizer for 5-field cron expressions. Recognizes only the
/// common numeric shapes; anything else falls back to the raw cron string:
///
/// - `M H * * *` -> `daily · HH:MM`
/// - `M * * * *` -> `hourly`
/// - `M H * * D` -> `weekly · <Day>` (0 and 7 both mean Sunday)
pub(crate) fn humanize_cron(cron: &str) -> String {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields.as_slice() else {
        return cron.to_string();
    };
    let num = |s: &str| s.parse::<u32>().ok();
    if let Some(m) = num(minute).filter(|m| *m <= 59) {
        if *hour == "*" && *dom == "*" && *month == "*" && *dow == "*" {
            return "hourly".to_string();
        }
        if let Some(h) = num(hour).filter(|h| *h <= 23)
            && *dom == "*"
            && *month == "*"
        {
            if *dow == "*" {
                return format!("daily · {h:02}:{m:02}");
            }
            if let Some(day) = num(dow).and_then(day_name) {
                return format!("weekly · {day}");
            }
        }
    }
    cron.to_string()
}

fn day_name(dow: u32) -> Option<&'static str> {
    Some(match dow {
        0 | 7 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        6 => "Sat",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{humanize_cron, slugify_flow_id};

    #[test]
    fn humanize_recognizes_daily_hourly_weekly() {
        assert_eq!(humanize_cron("0 9 * * *"), "daily · 09:00");
        assert_eq!(humanize_cron("30 17 * * *"), "daily · 17:30");
        assert_eq!(humanize_cron("15 * * * *"), "hourly");
        assert_eq!(humanize_cron("0 8 * * 1"), "weekly · Mon");
        assert_eq!(humanize_cron("0 8 * * 0"), "weekly · Sun");
        assert_eq!(humanize_cron("0 8 * * 7"), "weekly · Sun");
    }

    #[test]
    fn humanize_falls_back_to_raw_cron() {
        for raw in [
            "*/5 * * * *",
            "0 9 1 * *",
            "0 9 * 6 *",
            "0 9 * * MON",
            "0 99 * * *",
            "not a cron",
        ] {
            assert_eq!(humanize_cron(raw), raw);
        }
    }

    #[test]
    fn slugify_produces_model_style_ids() {
        assert_eq!(
            slugify_flow_id("  My Fancy Flow!  ").as_deref(),
            Some("my_fancy_flow")
        );
        assert_eq!(
            slugify_flow_id("Council Alert Pipeline").as_deref(),
            Some("council_alert_pipeline")
        );
        assert_eq!(slugify_flow_id("123 Go").as_deref(), Some("go"));
        assert_eq!(slugify_flow_id("Flow 42").as_deref(), Some("flow_42"));
        assert_eq!(slugify_flow_id("!!!"), None);
        assert_eq!(slugify_flow_id(""), None);
        let long = slugify_flow_id(&"x".repeat(100)).expect("slug");
        assert_eq!(long.len(), 64);
    }
}
