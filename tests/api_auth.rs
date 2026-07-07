//! Integration tests for human authentication: `/api/auth/*` endpoints and the
//! session guard over the data API. Each test builds the real `/api` router
//! over a temp database and drives it with `tower::ServiceExt::oneshot`.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use orchestrator::api::{self, AppState};
use orchestrator::auth::hash_password;
use orchestrator::db::Db;
use orchestrator::engine::Engine;
use orchestrator::scheduler::{RunLauncher, Scheduler, SystemClock};
use orchestrator::secrets::SecretStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct NoopLauncher;
impl RunLauncher for NoopLauncher {
    fn launch(&self, _run_id: i64) {}
}

struct Env {
    _dir: TempDir,
    db: Db,
    app: Router,
}

fn new_env() -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("orchestrator.db");
    let db = Db::open(&db_path).expect("open db");
    let pool = r2d2::Pool::builder()
        .max_size(2)
        .build(r2d2_sqlite::SqliteConnectionManager::file(&db_path))
        .expect("build secrets pool");
    let secrets =
        Arc::new(SecretStore::open(&dir.path().join("master.key"), pool).expect("open secrets"));
    let registry = Arc::new(orchestrator::plugins::testing::http_registry());
    let engine = Engine::new(db.clone(), Arc::clone(&registry), Arc::clone(&secrets));
    let scheduler = Scheduler::new(db.clone(), Arc::new(NoopLauncher), Arc::new(SystemClock));
    let state = AppState {
        db: db.clone(),
        engine,
        registry,
        secrets,
        scheduler,
        worker_tokens: Arc::new(vec![]),
    };
    let app = api::router(state).merge(orchestrator::ui::router());
    Env { _dir: dir, db, app }
}

/// Seed an admin user with a real argon2 hash (table must be empty).
fn seed_user(env: &Env, username: &str, password: &str) {
    env.db
        .create_first_user(username, &hash_password(password).unwrap())
        .unwrap()
        .expect("empty users table");
}

/// Send a request with optional JSON body and extra headers; returns status,
/// response headers, and text body.
async fn raw(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let body = body
        .map(|v| Body::from(v.to_string()))
        .unwrap_or_else(Body::empty);
    let request = builder.body(body).expect("build request");
    let response = app.clone().oneshot(request).await.expect("send request");
    let status = response.status();
    let resp_headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf-8 body");
    (status, resp_headers, text)
}

fn parse_json(text: &str) -> Value {
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(text).unwrap_or_else(|_| panic!("non-JSON body: {text}"))
    }
}

/// Extract the `orch_session` token from a `set-cookie` header.
fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let sc = headers.get(header::SET_COOKIE)?.to_str().ok()?;
    sc.split(';')
        .next()?
        .trim()
        .strip_prefix("orch_session=")
        .map(str::to_string)
}

/// Log in and return the `Cookie` header value for authenticated requests.
async fn login_cookie(env: &Env, username: &str, password: &str) -> String {
    let (status, headers, _) = raw(
        &env.app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": username, "password": password })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login should succeed");
    let token = cookie_token(&headers).expect("session cookie");
    format!("orch_session={token}")
}

#[tokio::test]
async fn login_ok_sets_httponly_cookie() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let (status, headers, text) = raw(
        &env.app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": "mike", "password": "pw" })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&text)["username"], "mike");
    let sc = headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(sc.starts_with("orch_session="), "cookie name: {sc}");
    assert!(sc.contains("HttpOnly"), "cookie must be HttpOnly: {sc}");
    assert!(sc.contains("SameSite=Lax"), "cookie samesite: {sc}");
}

#[tokio::test]
async fn login_bad_password_401() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let (status, _, text) = raw(
        &env.app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": "mike", "password": "wrong" })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(parse_json(&text)["error"], "invalid credentials");
}

#[tokio::test]
async fn login_unknown_user_401_not_500() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let (status, _, _) = raw(
        &env.app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": "ghost", "password": "pw" })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_requires_cookie() {
    let env = new_env();
    let (status, _, _) = raw(&env.app, "GET", "/api/auth/me", None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_with_cookie_returns_username() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let cookie = login_cookie(&env, "mike", "pw").await;
    let (status, _, text) = raw(
        &env.app,
        "GET",
        "/api/auth/me",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&text)["username"], "mike");
}

#[tokio::test]
async fn logout_clears_session() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let cookie = login_cookie(&env, "mike", "pw").await;
    let (status, _, _) = raw(
        &env.app,
        "POST",
        "/api/auth/logout",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The session is now gone: me with the same cookie is unauthorized.
    let (status, _, _) = raw(
        &env.app,
        "GET",
        "/api/auth/me",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn secure_flag_gated_on_forwarded_proto() {
    let env = new_env();
    seed_user(&env, "mike", "pw");

    let (_, https_headers, _) = raw(
        &env.app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": "mike", "password": "pw" })),
        &[("x-forwarded-proto", "https")],
    )
    .await;
    let https_cookie = https_headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        https_cookie.contains("Secure"),
        "https → Secure: {https_cookie}"
    );

    let (_, http_headers, _) = raw(
        &env.app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": "mike", "password": "pw" })),
        &[],
    )
    .await;
    let http_cookie = http_headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        !http_cookie.contains("Secure"),
        "http → no Secure: {http_cookie}"
    );
}

// --- guard over the data API (Task 5) --------------------------------------

#[tokio::test]
async fn guard_blocks_anonymous_data_request() {
    let env = new_env();
    let (status, _, text) = raw(&env.app, "GET", "/api/flows", None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(parse_json(&text)["error"], "unauthorized");
}

#[tokio::test]
async fn guard_allows_authenticated_data_request() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let cookie = login_cookie(&env, "mike", "pw").await;
    let (status, _, _) = raw(&env.app, "GET", "/api/flows", None, &[("cookie", &cookie)]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn health_is_public() {
    let env = new_env();
    // /api/health is registered in main, not api::router; the workers status
    // endpoint stands in for "public read under the guarded router's nest".
    let (status, _, _) = raw(&env.app, "GET", "/api/workers", None, &[]).await;
    assert_eq!(status, StatusCode::OK, "worker status stays public");
}

// --- first-run onboarding / setup ------------------------------------------

#[tokio::test]
async fn setup_needed_true_when_no_users() {
    let env = new_env();
    let (status, _, text) = raw(&env.app, "GET", "/api/auth/setup", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&text)["needed"], true);
}

#[tokio::test]
async fn setup_creates_admin_logs_in_and_closes() {
    let env = new_env();
    let (status, headers, text) = raw(
        &env.app,
        "POST",
        "/api/auth/setup",
        Some(json!({ "username": "mike", "password": "correct horse" })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&text)["username"], "mike");
    let token = cookie_token(&headers).expect("session cookie set on setup");
    let cookie = format!("orch_session={token}");

    // Setup is now closed and the caller is authenticated.
    let (_, _, needed) = raw(&env.app, "GET", "/api/auth/setup", None, &[]).await;
    assert_eq!(parse_json(&needed)["needed"], false);
    let (status, _, me) = raw(
        &env.app,
        "GET",
        "/api/auth/me",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&me)["username"], "mike");
}

#[tokio::test]
async fn setup_rejects_once_configured() {
    let env = new_env();
    seed_user(&env, "mike", "pw");
    let (status, _, text) = raw(
        &env.app,
        "POST",
        "/api/auth/setup",
        Some(json!({ "username": "intruder", "password": "correct horse" })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The existing admin is untouched.
    assert_eq!(parse_json(&text)["error"], "setup already completed");
}

#[tokio::test]
async fn setup_validates_username_and_password() {
    let env = new_env();
    let (short, _, _) = raw(
        &env.app,
        "POST",
        "/api/auth/setup",
        Some(json!({ "username": "mike", "password": "short" })),
        &[],
    )
    .await;
    assert_eq!(short, StatusCode::BAD_REQUEST);

    let (blank, _, _) = raw(
        &env.app,
        "POST",
        "/api/auth/setup",
        Some(json!({ "username": "   ", "password": "correct horse" })),
        &[],
    )
    .await;
    assert_eq!(blank, StatusCode::BAD_REQUEST);
    // Nothing was created by the rejected attempts.
    let (_, _, needed) = raw(&env.app, "GET", "/api/auth/setup", None, &[]).await;
    assert_eq!(parse_json(&needed)["needed"], true);
}
