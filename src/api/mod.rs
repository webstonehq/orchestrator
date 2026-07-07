//! JSON API served under `/api`.
//!
//! Route handlers live in submodules: [`flows`] (flows CRUD, validate,
//! revisions, import/export), [`misc`] (plugins, dashboard, schedules,
//! secrets), and [`runs`] (run lifecycle, logs, fan-out items, SSE).
//!
//! Errors use [`ApiError`]: every failure serializes as `{"error": "..."}`
//! with an appropriate status code, matching the UI client's expectations.

pub mod auth;
pub mod flows;
pub mod misc;
pub mod runs;
pub mod worker;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
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
///
/// Human data endpoints (flows/misc/runs) sit behind [`require_session`]; the
/// auth endpoints are the public login surface, and the worker API keeps its
/// own bearer-token auth. `/api/health` is registered in `main`, outside this
/// router.
pub fn router(state: AppState) -> Router {
    let guarded = Router::new()
        .merge(flows::router())
        .merge(misc::router())
        .merge(runs::router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    let open = Router::new().merge(auth::router()).merge(worker::router());

    Router::new().nest("/api", guarded.merge(open).with_state(state))
}

/// Require a valid human session cookie. Applied only to the human data routers
/// via `route_layer`; the worker API and `/api/auth/*` are exempt.
async fn require_session(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let authed = match auth::read_session_cookie(req.headers()) {
        Some(token) => matches!(state.db.session_username(&token), Ok(Some(_))),
        None => false,
    };
    if authed {
        next.run(req).await
    } else {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
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
