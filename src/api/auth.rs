//! Human authentication API (`/api/auth`): login, logout, and the `me` session
//! probe. These are public routes (exempt from the session guard) — they are
//! the login surface itself.
//!
//! Sessions are server-side rows keyed by an opaque, high-entropy token; the
//! client only ever holds that token in an HttpOnly cookie and never reads it
//! from JavaScript. This scheme is entirely separate from the worker
//! bearer-token auth: a human session grants no worker access and vice-versa.

use std::sync::OnceLock;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::{ApiError, AppState};
use crate::auth::{hash_password, new_session_token, verify_password};

const COOKIE_NAME: &str = "orch_session";
const SESSION_TTL_SECS: i64 = 2_592_000; // 30 days

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/setup", get(setup_needed).post(setup))
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

/// A valid argon2 hash of a throwaway password, computed once. Verifying an
/// unknown user's password against this keeps login timing roughly constant
/// whether or not the username exists (no early return before hashing).
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("orchestrator-dummy-password").expect("hash dummy"))
}

/// Whether the request arrived over HTTPS. Railway (and any TLS-terminating
/// proxy) forwards `X-Forwarded-Proto: https`. Gates the cookie's `Secure`
/// flag so a plain-http local run isn't locked out by a dropped cookie.
fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

fn session_cookie(token: &str, secure: bool, max_age: i64) -> String {
    let mut c = format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Read the `orch_session` value out of the `Cookie` request header.
pub fn read_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE_NAME)
        .map(|(_, v)| v.to_string())
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response, ApiError> {
    let stored = state.db.get_user_hash(&body.username)?;
    let ok = match &stored {
        Some(h) => verify_password(&body.password, h),
        None => {
            // Run a verify against a dummy hash so response timing doesn't
            // reveal whether the username exists.
            let _ = verify_password(&body.password, dummy_hash());
            false
        }
    };
    if !ok {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid credentials",
        ));
    }

    let uid = state
        .db
        .get_user_id(&body.username)?
        .expect("user verified above");
    let _ = state.db.sweep_expired_sessions();
    start_session(&state, uid, &body.username, &headers)
}

/// Mint a session for `user_id` and return a `200 {username}` response that sets
/// the session cookie. Shared by login and first-run setup.
fn start_session(
    state: &AppState,
    user_id: i64,
    username: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let token = new_session_token();
    state.db.create_session(&token, user_id, SESSION_TTL_SECS)?;
    let cookie = session_cookie(&token, is_https(headers), SESSION_TTL_SECS);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "username": username })),
    )
        .into_response())
}

#[derive(Deserialize)]
struct SetupBody {
    username: String,
    password: String,
}

/// Whether first-run setup is still available (no user exists yet). Public so
/// the SPA can choose between the onboarding and login screens.
async fn setup_needed(State(state): State<AppState>) -> Result<Response, ApiError> {
    Ok(Json(json!({ "needed": !state.db.has_users()? })).into_response())
}

/// Create the first admin account and log them in. Only succeeds while no user
/// exists; once setup is done this returns 409 so a public URL can't be
/// hijacked after the owner has onboarded.
async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetupBody>,
) -> Result<Response, ApiError> {
    let username = body.username.trim();
    if username.is_empty() {
        return Err(ApiError::bad_request("username is required"));
    }
    if body.password.len() < 8 {
        return Err(ApiError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    let hash = hash_password(&body.password).map_err(ApiError::internal)?;
    match state.db.create_first_user(username, &hash)? {
        Some(uid) => start_session(&state, uid, username, &headers),
        None => Err(ApiError::conflict("setup already completed")),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = read_session_cookie(&headers) {
        let _ = state.db.delete_session(&tok);
    }
    // Expire the cookie regardless.
    let cookie = session_cookie("", is_https(&headers), 0);
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let token = read_session_cookie(&headers)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    match state.db.session_username(&token)? {
        Some(username) => Ok(Json(json!({ "username": username })).into_response()),
        None => Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}
