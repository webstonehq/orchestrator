//! Plugins, dashboard, schedules, and secrets routes.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::FlowDefinition;
use crate::plugins::PluginManifest;
use crate::scheduler::SchedulerError;
use crate::secrets::SecretsError;

use super::flows::humanize_cron;
use super::{ApiError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/plugins", get(plugins))
        .route("/flow.schema.json", get(flow_schema))
        .route("/dashboard", get(dashboard))
        .route("/schedules", get(schedules))
        .route("/schedules/{flow}/{trigger}/toggle", post(toggle_schedule))
        .route("/secrets", get(list_secrets))
        .route("/secrets/{name}", put(put_secret).delete(delete_secret))
}

#[derive(Deserialize)]
struct ToggleBody {
    enabled: bool,
}

#[derive(Deserialize)]
struct SecretBody {
    value: String,
}

async fn plugins(State(state): State<AppState>) -> Json<Vec<PluginManifest>> {
    Json(state.registry.manifests())
}

/// The flow JSON Schema, assembled from the live plugin registry. The YAML
/// editor fetches this for autocomplete, hover docs, and inline validation, so
/// it always reflects the plugins installed in *this* binary.
async fn flow_schema(State(state): State<AppState>) -> Json<Value> {
    Json(crate::model::flow_json_schema(&state.registry))
}

async fn dashboard(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let metrics = state.db.dashboard_metrics()?;
    // Soonest upcoming fire across enabled schedules of unpaused flows.
    // Timestamps are RFC3339 UTC with fixed precision, so the lexicographic
    // minimum is the chronological minimum.
    let next_scheduled = state
        .db
        .list_schedules()?
        .into_iter()
        .filter(|row| row.enabled && !row.flow_paused)
        .filter_map(|row| row.next_fire_at.map(|at| (row.flow_id, at)))
        .min_by(|a, b| a.1.cmp(&b.1))
        .map(|(flow_id, at)| json!({ "flow_id": flow_id, "at": at }));
    Ok(Json(json!({
        "active_flows": metrics.active_flows,
        "runs_24h": metrics.runs_24h,
        "success_rate_30d": metrics.success_rate_30d,
        "avg_duration_sec": metrics.avg_duration_sec_30d,
        "next_scheduled": next_scheduled,
    })))
}

async fn schedules(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let stats = state.db.flow_run_stats()?;
    // Cron/timezone/catchup come from the flow's *current* definition;
    // schedule_state only owns enabled/next/last. Unparseable definitions
    // (should not happen) and state rows whose trigger vanished are skipped.
    let mut defs: HashMap<String, FlowDefinition> = HashMap::new();
    for flow in state.db.list_flows()? {
        if let Ok(def) = serde_json::from_str::<FlowDefinition>(&flow.definition) {
            defs.insert(flow.id, def);
        }
    }
    let mut out = Vec::new();
    for row in state.db.list_schedules()? {
        let Some(trigger) = defs
            .get(&row.flow_id)
            .and_then(|def| def.triggers.iter().find(|t| t.id == row.trigger_id))
        else {
            continue;
        };
        out.push(json!({
            "flow_id": row.flow_id,
            "flow_name": row.flow_name,
            "trigger_id": row.trigger_id,
            "cron": trigger.cron,
            "timezone": trigger.timezone,
            "human": humanize_cron(&trigger.cron),
            "catchup": trigger.catchup,
            "enabled": row.enabled,
            "next_fire_at": row.next_fire_at,
            "last_fired_at": row.last_fired_at,
            "last_run_status": stats.get(&row.flow_id).and_then(|s| s.last_run_status.clone()),
        }));
    }
    Ok(Json(Value::Array(out)))
}

async fn toggle_schedule(
    State(state): State<AppState>,
    Path((flow, trigger)): Path<(String, String)>,
    Json(body): Json<ToggleBody>,
) -> Result<StatusCode, ApiError> {
    // set_enabled pokes the scheduler's notify itself.
    match state.scheduler.set_enabled(&flow, &trigger, body.enabled) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e @ (SchedulerError::UnknownFlow(_) | SchedulerError::UnknownTrigger { .. })) => {
            Err(ApiError::new(StatusCode::NOT_FOUND, e.to_string()))
        }
        Err(e) => Err(ApiError::internal(e)),
    }
}

async fn list_secrets(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    // Metadata only; SecretMeta carries no value field by design.
    let metas = state.secrets.list()?;
    let out: Vec<Value> = metas
        .into_iter()
        .map(|m| {
            json!({
                "name": m.name,
                "created_at": m.created_at,
                "updated_at": m.updated_at,
            })
        })
        .collect();
    Ok(Json(Value::Array(out)))
}

async fn put_secret(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<SecretBody>,
) -> Result<StatusCode, ApiError> {
    match state.secrets.set(&name, &body.value) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e @ SecretsError::InvalidName(_)) => Err(ApiError::bad_request(e.to_string())),
        Err(e) => Err(ApiError::internal(e)),
    }
}

async fn delete_secret(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !state.secrets.delete(&name)? {
        return Err(ApiError::not_found(format!("secret `{name}`")));
    }
    Ok(StatusCode::NO_CONTENT)
}
