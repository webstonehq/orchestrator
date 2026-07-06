//! JSON API served under `/api`.
//!
//! Route handlers live in submodules: [`flows`] (flows CRUD, validate,
//! revisions, import/export), [`misc`] (plugins, dashboard, schedules,
//! secrets), and [`runs`] (run lifecycle, logs, fan-out items, SSE).
//!
//! Errors use [`ApiError`]: every failure serializes as `{"error": "..."}`
//! with an appropriate status code, matching the UI client's expectations.

pub mod flows;
pub mod misc;
pub mod runs;
pub mod worker;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::db::Db;
use crate::engine::Engine;
use crate::plugins::PluginRegistry;
use crate::scheduler::Scheduler;
use crate::secrets::SecretStore;

/// Shared state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub engine: Arc<Engine>,
    pub registry: Arc<PluginRegistry>,
    pub secrets: Arc<SecretStore>,
    pub scheduler: Arc<Scheduler>,
    /// Accepted worker bearer tokens. Empty disables the worker API (every
    /// request 401s), which is the default single-node posture.
    pub worker_tokens: Arc<Vec<String>>,
}

/// The full `/api` router. The UI router is merged separately in `main`.
pub fn router(state: AppState) -> Router {
    Router::new().nest(
        "/api",
        Router::new()
            .merge(flows::router())
            .merge(misc::router())
            .merge(runs::router())
            .merge(worker::router())
            .with_state(state),
    )
}

/// API error: status code + human message, serialized as `{"error": msg}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    pub fn internal(err: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = axum::Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

impl From<crate::db::DbError> for ApiError {
    fn from(e: crate::db::DbError) -> Self {
        ApiError::internal(e)
    }
}

impl From<crate::secrets::SecretsError> for ApiError {
    fn from(e: crate::secrets::SecretsError) -> Self {
        ApiError::internal(e)
    }
}
