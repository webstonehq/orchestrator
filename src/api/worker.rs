//! Worker control-plane API (`/api/worker`).
//!
//! Pull-based and worker-initiated: a worker dials in, claims queued runs off
//! its queues, executes them against its own engine, and streams the resulting
//! `RunUpdate`s back for the server to persist and rebroadcast. Every request
//! carries a bearer token checked against [`AppState::worker_tokens`]; an empty
//! allowlist disables the API entirely (the default single-node posture).
//!
//! Routes:
//! - `POST /api/worker/claim` — lease up to `capacity` runs; returns
//!   [`Assignment`]s (flow definition + inputs, no secrets).
//! - `POST /api/worker/updates` — apply a batch of sequenced updates for one
//!   run; replies whether the run has since been cancelled.
//! - `POST /api/worker/heartbeat` — renew leases; returns the ids the server
//!   wants cancelled.

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::engine::{Assignment, SeqUpdate};

use super::{ApiError, AppState};

/// Default lease length granted on claim/heartbeat. Comfortably longer than
/// the worker's heartbeat cadence so a slow beat never drops a live run.
const LEASE_SECS: i64 = 30;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/worker/claim", post(claim))
        .route("/worker/updates", post(updates))
        .route("/worker/heartbeat", post(heartbeat))
        // Read-only status for the UI panel (no worker token required — same
        // posture as the rest of the local read API).
        .route("/workers", axum::routing::get(workers_status))
}

#[derive(Serialize)]
struct WorkersResponse {
    /// Whether any worker tokens are configured. Lets the UI tell "workers
    /// disabled" apart from "none connected".
    enabled: bool,
    workers: Vec<crate::engine::WorkerStatus>,
}

async fn workers_status(State(state): State<AppState>) -> Result<Json<WorkersResponse>, ApiError> {
    let workers = state.engine.worker_statuses().map_err(ApiError::internal)?;
    Ok(Json(WorkersResponse {
        enabled: !state.worker_tokens.is_empty(),
        workers,
    }))
}

/// Authorize a worker request: `Authorization: Bearer <token>` must match an
/// entry in the allowlist. An empty allowlist rejects everything.
fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    match token {
        Some(t) if state.worker_tokens.iter().any(|allowed| allowed == t) => Ok(()),
        _ => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid or missing worker token",
        )),
    }
}

#[derive(Deserialize)]
struct ClaimBody {
    worker_id: String,
    queues: Vec<String>,
    capacity: u32,
}

#[derive(Serialize)]
struct ClaimResponse {
    assignments: Vec<Assignment>,
}

async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ClaimBody>,
) -> Result<Json<ClaimResponse>, ApiError> {
    authorize(&state, &headers)?;
    let queues: Vec<&str> = body.queues.iter().map(String::as_str).collect();
    let assignments = state
        .engine
        .claim_remote(&body.worker_id, &queues, body.capacity, LEASE_SECS)
        .map_err(ApiError::internal)?;
    Ok(Json(ClaimResponse { assignments }))
}

#[derive(Deserialize)]
struct UpdatesBody {
    worker_id: String,
    run_id: i64,
    updates: Vec<SeqUpdate>,
}

#[derive(Serialize)]
struct UpdatesResponse {
    /// Whether the run has been cancelled server-side (the worker should stop
    /// it). Set once any update reveals the run is no longer active.
    canceled: bool,
}

async fn updates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdatesBody>,
) -> Result<Json<UpdatesResponse>, ApiError> {
    authorize(&state, &headers)?;
    for SeqUpdate { seq, update } in body.updates {
        state
            .engine
            .apply_remote_update(body.run_id, seq, update)
            .map_err(ApiError::internal)?;
    }
    // Renew the lease (a run producing updates is alive) and report whether
    // it has since been cancelled server-side.
    let canceled = state
        .engine
        .heartbeat_remote(&body.worker_id, &[body.run_id], LEASE_SECS)
        .map_err(ApiError::internal)?
        .contains(&body.run_id);
    Ok(Json(UpdatesResponse { canceled }))
}

#[derive(Deserialize)]
struct HeartbeatBody {
    worker_id: String,
    run_ids: Vec<i64>,
}

#[derive(Serialize)]
struct HeartbeatResponse {
    canceled: Vec<i64>,
}

async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HeartbeatBody>,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    authorize(&state, &headers)?;
    let canceled = state
        .engine
        .heartbeat_remote(&body.worker_id, &body.run_ids, LEASE_SECS)
        .map_err(ApiError::internal)?;
    Ok(Json(HeartbeatResponse { canceled }))
}
