# Authentication Implementation Plan

> **⚠️ Partially superseded (2026-07-07):** the admin is created via a **first-run
> onboarding screen**, not seeded from `ORCH_ADMIN_USER`/`ORCH_ADMIN_PASSWORD`.
> Task 3 (env seeding) and the env-var references in Tasks 4/10 were replaced by
> `Db::{has_users,create_first_user,get_user_id}`, `GET/POST /api/auth/setup`,
> and a `Setup.svelte` onboarding view. The as-built design is in
> `2026-07-07-auth-design.md`; this plan is kept as the historical scaffold.
>
> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task, and superpowers:test-driven-development within each task.
>
> **Git note (repo owner's rule):** Claude never runs git operations here. The
> "Commit" steps below are for the human operator (Mike). Claude implements +
> runs tests per task and pauses for review; Mike commits.

**Goal:** Add username/password authentication (env-seeded admin, server-side
sessions, HTTP-only cookie) gating the human-facing UI and JSON API, so the
public Railway deployment is safe for a single operator.

**Architecture:** A new top-level `auth` module owns password hashing (argon2id),
session-token generation, and cookie construction. New `users`/`sessions` SQLite
tables (migration v5) with `Db` helpers. On `serve` startup the admin is
upserted from `ORCH_ADMIN_USER`/`ORCH_ADMIN_PASSWORD`. Three public `/api/auth/*`
endpoints (login/logout/me) plus an axum middleware guard over the
flows/misc/runs routers; the worker API keeps its own bearer auth. The SvelteKit
SPA gates itself at the root layout via `/api/auth/me`, shows a login view, and
falls back to it on any `401`.

**Tech Stack:** Rust (axum 0.8, rusqlite, argon2), SvelteKit 5 (Svelte runes).

Design reference: `docs/plans/2026-07-07-auth-design.md`.

---

## Shared contracts (read first)

**Endpoints (all JSON, under `/api`):**

- `POST /api/auth/login` — req `{"username","password"}`; res `200 {"username"}`
  + `Set-Cookie`, or `401 {"error":"invalid credentials"}`.
- `POST /api/auth/logout` — no body; res `200 {"ok":true}`, clears cookie.
  Idempotent.
- `GET /api/auth/me` — res `200 {"username"}` or `401 {"error":"unauthorized"}`.

**Cookie:** name `orch_session`; attributes `HttpOnly; SameSite=Lax; Path=/;
Max-Age=2592000` plus `Secure` iff request is HTTPS
(`X-Forwarded-Proto: https`). Logout sends `Max-Age=0`.

**Guard 401 body:** `{"error":"unauthorized"}`.

**Env vars:** `ORCH_ADMIN_USER`, `ORCH_ADMIN_PASSWORD` (both required to seed).

**Session TTL:** 2_592_000 s (30 days), fixed.

---

## Task 1: Password hashing + token module (`src/auth.rs`)

**Files:**
- Create: `src/auth.rs`
- Modify: `src/lib.rs` (add `pub mod auth;`), `Cargo.toml` (deps)

**Step 1 — deps.** Add to `Cargo.toml` `[dependencies]`:
```toml
argon2 = "0.5"
getrandom = "0.2"
```
Run `cargo build` to resolve. If `getrandom` 0.2 conflicts with the tree, use
whatever major already appears in `Cargo.lock`.

**Step 2 — write failing unit tests** (in `src/auth.rs` `#[cfg(test)]`):
```rust
#[test]
fn hash_then_verify_roundtrips() {
    let h = hash_password("hunter2").unwrap();
    assert!(h.starts_with("$argon2id$"));
    assert!(verify_password("hunter2", &h));
    assert!(!verify_password("wrong", &h));
}

#[test]
fn verify_rejects_garbage_hash_without_panicking() {
    assert!(!verify_password("x", "not-a-phc-string"));
}

#[test]
fn session_tokens_are_unique_and_long() {
    let a = new_session_token();
    let b = new_session_token();
    assert_ne!(a, b);
    assert_eq!(a.len(), 64); // 32 bytes hex
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}
```

**Step 3 — run, expect FAIL** (unresolved names):
`cargo test --lib auth::`

**Step 4 — implement `src/auth.rs`:**
```rust
//! Password hashing (argon2id) and session-token generation.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Hash a password with argon2id, returning a PHC string for storage.
pub fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).map_err(|e| e.to_string())?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Verify a password against a stored PHC hash. Any parse/verify failure → false.
pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A fresh 256-bit session token, hex-encoded (64 chars).
pub fn new_session_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS RNG");
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
```
Add `pub mod auth;` to `src/lib.rs` (alphabetical: after `pub mod api;`).

**Step 5 — run, expect PASS:** `cargo test --lib auth::`

**Step 6 — Commit (Mike):** `feat(auth): argon2 hashing + session tokens`

---

## Task 2: DB migration v5 + user/session helpers (`src/db.rs`)

**Files:**
- Modify: `src/db.rs` (migration const + `MIGRATIONS` array + `impl Db` helpers)
- Test: `tests/db.rs`

**Step 1 — migration.** Near the other `MIGRATION_00X` consts add:
```rust
const MIGRATION_005: &str = "\
CREATE TABLE users (
  id         INTEGER PRIMARY KEY,
  username   TEXT NOT NULL UNIQUE,
  pw_hash    TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE sessions (
  token      TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
";
```
Append `(5, MIGRATION_005),` to the `MIGRATIONS` array.

**Step 2 — write failing tests** in `tests/db.rs`:
```rust
#[test]
fn upsert_user_inserts_then_updates() {
    let (_d, db) = fresh();
    db.upsert_user("mike", "hash1").unwrap();
    assert_eq!(db.get_user_hash("mike").unwrap().as_deref(), Some("hash1"));
    db.upsert_user("mike", "hash2").unwrap(); // same username → update
    assert_eq!(db.get_user_hash("mike").unwrap().as_deref(), Some("hash2"));
    assert_eq!(db.get_user_hash("nobody").unwrap(), None);
}

#[test]
fn session_create_lookup_delete() {
    let (_d, db) = fresh();
    let uid = db.upsert_user("mike", "h").unwrap();
    db.create_session("tok", uid, 3600).unwrap();
    assert_eq!(db.session_username("tok").unwrap().as_deref(), Some("mike"));
    db.delete_session("tok").unwrap();
    assert_eq!(db.session_username("tok").unwrap(), None);
}

#[test]
fn expired_session_is_not_returned_and_sweeps() {
    let (_d, db) = fresh();
    let uid = db.upsert_user("mike", "h").unwrap();
    db.create_session("live", uid, 3600).unwrap();
    db.create_session("dead", uid, -10).unwrap(); // already expired
    assert_eq!(db.session_username("dead").unwrap(), None);
    assert_eq!(db.session_username("live").unwrap().as_deref(), Some("mike"));
    db.sweep_expired_sessions().unwrap();
    // 'dead' row physically gone; 'live' remains
    assert_eq!(db.session_username("live").unwrap().as_deref(), Some("mike"));
}
```
(Use the existing `tests/db.rs` temp-db helper; if none is named `fresh`, mirror
the file's existing setup pattern.)

**Step 3 — run, expect FAIL:** `cargo test --test db`

**Step 4 — implement helpers** in `impl Db` (near flows helpers). `upsert_user`
returns the row id; expiry is computed in Rust so the SQL stays trivial:
```rust
// -- auth: users & sessions --------------------------------------------

/// Insert a user, or update the password hash if the username exists.
/// Returns the user's id.
pub fn upsert_user(&self, username: &str, pw_hash: &str) -> DbResult<i64> {
    let conn = self.conn()?;
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO users (username, pw_hash, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT(username) DO UPDATE SET pw_hash = ?2, updated_at = ?3",
        params![username, pw_hash, now],
    )?;
    let id = conn.query_row(
        "SELECT id FROM users WHERE username = ?1", [username], |r| r.get(0),
    )?;
    Ok(id)
}

/// The stored argon2 hash for a username, if the user exists.
pub fn get_user_hash(&self, username: &str) -> DbResult<Option<String>> {
    let conn = self.conn()?;
    Ok(conn
        .query_row("SELECT pw_hash FROM users WHERE username = ?1", [username],
            |r| r.get(0))
        .optional()?)
}

/// Create a session valid for `ttl_secs` from now.
pub fn create_session(&self, token: &str, user_id: i64, ttl_secs: i64) -> DbResult<()> {
    let conn = self.conn()?;
    let now = chrono::Utc::now();
    let created = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let expires = (now + chrono::Duration::seconds(ttl_secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute(
        "INSERT INTO sessions (token, user_id, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![token, user_id, created, expires],
    )?;
    Ok(())
}

/// Username for a live (unexpired) session token, if any.
pub fn session_username(&self, token: &str) -> DbResult<Option<String>> {
    let conn = self.conn()?;
    Ok(conn
        .query_row(
            "SELECT u.username FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token = ?1 AND s.expires_at > ?2",
            params![token, now_rfc3339()],
            |r| r.get(0),
        )
        .optional()?)
}

/// Delete a session (logout). Ok whether or not it existed.
pub fn delete_session(&self, token: &str) -> DbResult<()> {
    let conn = self.conn()?;
    conn.execute("DELETE FROM sessions WHERE token = ?1", [token])?;
    Ok(())
}

/// Delete all expired session rows (opportunistic cleanup).
pub fn sweep_expired_sessions(&self) -> DbResult<()> {
    let conn = self.conn()?;
    conn.execute("DELETE FROM sessions WHERE expires_at < ?1", [now_rfc3339()])?;
    Ok(())
}
```

**Step 5 — run, expect PASS:** `cargo test --test db`

**Step 6 — Commit (Mike):** `feat(auth): users/sessions schema + Db helpers`

---

## Task 3: Seed admin from env on startup (`src/main.rs`)

**Files:**
- Modify: `src/main.rs` (in `serve`, after `Db::open`, before building the router)
- Test: `tests/db.rs` (a small helper-level test; the env read itself is glue)

**Step 1 — implement seeding** in `serve()`, right after the `let db = Db::open(...)?;`
line:
```rust
// Seed/refresh the admin from env: env is the source of truth. Both vars
// must be set; otherwise no-op (empty users table → every login 401s until
// configured).
match (std::env::var("ORCH_ADMIN_USER"), std::env::var("ORCH_ADMIN_PASSWORD")) {
    (Ok(user), Ok(pass)) if !user.is_empty() && !pass.is_empty() => {
        let hash = orchestrator::auth::hash_password(&pass)
            .map_err(|e| std::io::Error::other(format!("hash admin password: {e}")))?;
        db.upsert_user(&user, &hash)?;
        tracing::info!(user = %user, "admin account seeded/updated from env");
    }
    _ => {
        tracing::warn!(
            "no ORCH_ADMIN_USER/ORCH_ADMIN_PASSWORD set; login disabled until configured"
        );
    }
}
```
(Confirm `Db`/io error conversions compile; `serve` returns
`Result<(), Box<dyn std::error::Error>>`, so `?` on `DbError`/`io::Error` is fine.)

**Step 2 — sanity test** (proves upsert-from-hash path end to end at the Db
layer; the env plumbing is exercised by manual/integration run in Task 9):
```rust
#[test]
fn seeded_admin_hash_verifies() {
    let (_d, db) = fresh();
    let hash = orchestrator::auth::hash_password("s3cret").unwrap();
    db.upsert_user("admin", &hash).unwrap();
    let stored = db.get_user_hash("admin").unwrap().unwrap();
    assert!(orchestrator::auth::verify_password("s3cret", &stored));
    assert!(!orchestrator::auth::verify_password("nope", &stored));
}
```

**Step 3 — run:** `cargo test --test db seeded_admin_hash_verifies` → PASS.
**Step 4 — build:** `cargo build` → clean.
**Step 5 — Commit (Mike):** `feat(auth): seed admin from env on startup`

---

## Task 4: Auth endpoints (`src/api/auth.rs`)

**Files:**
- Create: `src/api/auth.rs`
- Modify: `src/api/mod.rs` (`pub mod auth;`)
- Test: `tests/api_auth.rs` (new; copy the harness from `tests/api_flows.rs`)

**Step 1 — write failing integration tests** `tests/api_auth.rs`. Reuse
`new_env()`-style harness (temp db, real router). Seed a user directly via
`env.db.upsert_user("mike", &hash_password("pw"))`. Cases:
```
- login_ok:        POST /api/auth/login {mike,pw} → 200, body has "username":"mike",
                   response has a Set-Cookie starting "orch_session="; HttpOnly present.
- login_bad_pw:    POST .../login {mike,wrong} → 401, body {"error":"invalid credentials"}.
- login_unknown:   POST .../login {ghost,pw}   → 401 (and does not 500).
- me_requires_cookie: GET /api/auth/me with no cookie → 401.
- me_with_cookie:  login → capture cookie token → GET /api/auth/me with
                   "Cookie: orch_session=<tok>" → 200 "username":"mike".
- logout_clears:   login → POST /api/auth/logout with cookie → 200; then
                   GET /api/auth/me with same cookie → 401.
- secure_flag:     POST /api/auth/login with header "X-Forwarded-Proto: https"
                   → Set-Cookie contains "Secure"; without the header → no "Secure".
```
Add a small helper to pull the token out of the `set-cookie` header.

**Step 2 — run, expect FAIL** (`api_auth` module/route missing):
`cargo test --test api_auth`

**Step 3 — implement `src/api/auth.rs`:**
```rust
//! Human authentication API (`/api/auth`): login, logout, and the `me`
//! session probe. Public routes (no session guard) — this is the login
//! surface itself. Sessions are server-side rows; the client holds only an
//! opaque HttpOnly cookie. Distinct from the worker bearer-token scheme.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{hash_password, new_session_token, verify_password};
use super::{ApiError, AppState};

const COOKIE_NAME: &str = "orch_session";
const SESSION_TTL_SECS: i64 = 2_592_000; // 30 days

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

/// Whether the request arrived over HTTPS (Railway/any TLS-terminating proxy
/// forwards `X-Forwarded-Proto: https`). Gates the cookie's `Secure` flag so a
/// plain-http local run isn't locked out by a dropped cookie.
fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

fn session_cookie(token: &str, secure: bool, max_age: i64) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}"
    );
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
    // Always run a verify (dummy hash for unknown users) so timing doesn't
    // reveal whether the username exists.
    const DUMMY: &str =
        "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$RdescudvJCsgt3ub+b+dWRWJTmaaJObG";
    let ok = match &stored {
        Some(h) => verify_password(&body.password, h),
        None => {
            let _ = verify_password(&body.password, DUMMY);
            false
        }
    };
    if !ok {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid credentials"));
    }
    let uid = state.db.upsert_user(&body.username, stored.as_deref().unwrap())?;
    let token = new_session_token();
    state.db.create_session(&token, uid, SESSION_TTL_SECS)?;
    let _ = state.db.sweep_expired_sessions();
    let cookie = session_cookie(&token, is_https(&headers), SESSION_TTL_SECS);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "username": body.username })),
    )
        .into_response())
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = read_session_cookie(&headers) {
        let _ = state.db.delete_session(&tok);
    }
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
```
Note: `upsert_user` in `login` is only to fetch the id cheaply and is a no-op
update; simpler alternative is a `get_user_id` helper — if you prefer, add
`get_user_id(username) -> Option<i64>` to `Db` and use it here instead of the
re-upsert. Either is fine; pick one and keep it.

Add `pub mod auth;` to `src/api/mod.rs`.

**Step 4 — run, expect PASS:** `cargo test --test api_auth`
**Step 5 — Commit (Mike):** `feat(auth): login/logout/me endpoints`

---

## Task 5: Session guard middleware + router wiring (`src/api/mod.rs`)

**Files:**
- Modify: `src/api/mod.rs` (guard fn + restructure `router`)
- Test: `tests/api_auth.rs` (guard cases), and confirm `tests/api_flows.rs` /
  `tests/api_runs.rs` still pass (they build the router — they will now need a
  session cookie; see Step 4).

**Step 1 — write failing guard tests** in `tests/api_auth.rs`:
```
- guard_blocks_anon:  GET /api/flows with no cookie → 401 {"error":"unauthorized"}.
- guard_allows_session: seed user, login, GET /api/flows with cookie → 200.
- health_is_public:   GET /api/health with no cookie → 200.
- worker_status_public_or_own_auth: GET /api/workers with no cookie → still 200
                      (unchanged behavior — workers panel).  # keep existing posture
```

**Step 2 — implement the guard** in `src/api/mod.rs`:
```rust
use axum::extract::Request;
use axum::middleware::{self, Next};

/// Require a valid human session cookie. Applied only to the human data
/// routers (flows/misc/runs); the worker API and /api/auth/* are exempt.
async fn require_session(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let token = auth::read_session_cookie(req.headers());
    let authed = match token {
        Some(t) => matches!(state.db.session_username(&t), Ok(Some(_))),
        None => false,
    };
    if authed {
        next.run(req).await
    } else {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}
```

**Step 3 — restructure `router`:**
```rust
pub fn router(state: AppState) -> Router {
    // Human data endpoints — behind the session guard.
    let guarded = Router::new()
        .merge(flows::router())
        .merge(misc::router())
        .merge(runs::router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    // Public/self-authenticating endpoints — no session guard.
    // auth::router is the login surface; worker::router keeps its bearer auth.
    let open = Router::new()
        .merge(auth::router())
        .merge(worker::router());

    Router::new().nest("/api", guarded.merge(open).with_state(state))
}
```
(`AppState` already derives `Clone`. `route_layer` applies the guard to the
merged routes but not the nest fallback, so unknown `/api/*` paths still hit the
UI router's JSON 404.)

**Step 4 — fix existing API tests.** `tests/api_flows.rs` and `tests/api_runs.rs`
now hit guarded routes. Add a shared login-and-attach-cookie helper to each
harness (or seed a user + session row directly and set the `Cookie` header on
every request). Minimal approach: in each harness, after building `db`, insert a
user + a known session token, and have the request helper attach
`Cookie: orch_session=<tok>` by default. Update both files.

**Step 5 — run the whole API suite:**
`cargo test --test api_auth --test api_flows --test api_runs` → all PASS.

**Step 6 — Commit (Mike):** `feat(auth): session guard over data API`

---

## Task 6: Backend gate check

**Step 1:** `cargo test` (full suite) → PASS.
**Step 2:** `cargo clippy --all-targets -- -D warnings` → clean.
**Step 3:** `cargo fmt --check` → clean.
**Step 4 — manual smoke (optional here, full run in Task 9):**
```
ORCH_ADMIN_USER=mike ORCH_ADMIN_PASSWORD=pw cargo run -- serve &
curl -i localhost:4400/api/flows            # → 401
curl -i -c jar -X POST localhost:4400/api/auth/login \
  -H 'content-type: application/json' -d '{"username":"mike","password":"pw"}'  # → 200 + cookie
curl -i -b jar localhost:4400/api/flows      # → 200
```
**Step 5 — Commit (Mike):** none (checkpoint only).

---

## Task 7: UI auth client + global 401 hook (`ui/src/lib/api.ts`)

**Files:**
- Modify: `ui/src/lib/api.ts`
- Test: `ui/src/lib/api.test.ts`

**Step 1 — write failing tests** (mirror existing `api.test.ts` fetch-mock
style):
```
- api.auth.login posts to /api/auth/login and returns {username}.
- api.auth.me returns {username} on 200.
- a 401 from any request invokes the registered onUnauthorized handler once.
```

**Step 2 — implement.** Add an auth surface and a 401 hook to the client:
```ts
// near the Authuser type section
export interface AuthUser { username: string; }

let onUnauthorized: (() => void) | null = null;
/** Register a callback fired whenever any API call returns 401. */
export function setUnauthorizedHandler(fn: () => void) { onUnauthorized = fn; }
```
In `request()`, right where a non-ok response is handled, before throwing:
```ts
if (res.status === 401) onUnauthorized?.();
```
Add to the `api` object:
```ts
auth: {
    me: () => get<AuthUser>('/api/auth/me'),
    login: (username: string, password: string) =>
        post<AuthUser>('/api/auth/login', { username, password }),
    logout: () => post<{ ok: boolean }>('/api/auth/logout')
},
```

**Step 3 — run:** `cd ui && npm test -- api` → PASS.
**Step 4 — Commit (Mike):** `feat(ui): auth API client + 401 hook`

---

## Task 8: Login view + layout gate (`ui/src/routes/+layout.svelte`)

**Files:**
- Create: `ui/src/lib/components/Login.svelte`
- Modify: `ui/src/routes/+layout.svelte`

**Step 1 — Login.svelte.** A centered card in the existing dark theme (reuse
`$lib/theme.css` tokens; accent `#7ee787`). Props: `onAuthed: () => void`.
Fields: username, password, submit. On submit call `api.auth.login`; on success
call `onAuthed()`; on `ApiError` (401) show inline "Invalid username or
password." Disable the button while pending.

**Step 2 — gate the layout.** In `+layout.svelte`:
```svelte
<script lang="ts">
  import { api, setUnauthorizedHandler, type AuthUser } from '$lib/api';
  import Login from '$lib/components/Login.svelte';
  // ...existing imports...

  let auth = $state<'loading' | 'in' | 'out'>('loading');
  let user = $state<AuthUser | null>(null);

  async function check() {
    try { user = await api.auth.me(); auth = 'in'; }
    catch { auth = 'out'; }
  }
  setUnauthorizedHandler(() => { auth = 'out'; user = null; });
  check();
</script>

{#if auth === 'loading'}
  <!-- minimal splash / nothing -->
{:else if auth === 'out'}
  <Login onAuthed={check} />
{:else}
  <!-- existing shell markup unchanged, wrapped here -->
{/if}
```
Keep all existing shell markup inside the `{:else}` branch. `check()` runs on
mount (module-level call in `<script>` is fine in a component instance).

**Step 3 — verify build:** `cd ui && npm run build` → succeeds; check
`ui/build/index.html` is regenerated.

**Step 4 — Commit (Mike):** `feat(ui): login view + layout auth gate`

---

## Task 9: Logout control + end-to-end verification

**Files:**
- Modify: `ui/src/routes/+layout.svelte` (top bar: "signed in as {user} · Log out")

**Step 1 — logout control.** In the shell top bar / sidebar footer add a button:
```svelte
<button class="logout" onclick={async () => { await api.auth.logout(); auth = 'out'; user = null; }}>
  Log out
</button>
```
Style to match existing controls; show `user.username` beside it.

**Step 2 — rebuild UI:** `cd ui && npm run build`.

**Step 3 — full-stack manual verification** (use the `verify` / `run` skill):
```
cd .. && ORCH_ADMIN_USER=mike ORCH_ADMIN_PASSWORD=pw cargo run -- serve
```
In a browser at `http://127.0.0.1:4400`:
- Unauthenticated load → login view (not the app).
- Wrong password → inline error, stays on login.
- Correct login → app renders; reload keeps you in (cookie persists).
- Log out → back to login; the app is gone.
- (Optional) delete the session row / wait out expiry → next API call drops to login.

**Step 4 — full test + lint gate:**
```
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
cd ui && npm test && npm run check && npm run build
```

**Step 5 — Commit (Mike):** `feat(ui): logout control` (+ any test updates).

---

## Task 10: Docs & Railway template

**Files:**
- Modify: `README.md` (auth section: env vars, first-login, rotation caveat),
  Railway template variables (`ORCH_ADMIN_USER`, `ORCH_ADMIN_PASSWORD` as
  prompted vars), `todo.txt` (strike the auth line).

**Step 1 — README:** document `ORCH_ADMIN_USER`/`ORCH_ADMIN_PASSWORD`, that an
empty+unconfigured deploy has login disabled, and the rotation model (change the
env var + redeploy; env wins each boot; a persisted password survives a later
unset).
**Step 2 — Railway template metadata:** add the two prompted variables (mark
`ORCH_ADMIN_PASSWORD` secret/generate).
**Step 3 — todo.txt:** remove/मark done the "Add auth" line.
**Step 4 — Commit (Mike):** `docs(auth): env vars + rotation; railway template`

---

## Definition of done

- `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check` all clean.
- `cd ui && npm test && npm run check && npm run build` all clean.
- Manual E2E: anon blocked, login works, session persists across reload, logout
  returns to login, guarded API 401s without a cookie, health + worker API
  unaffected.
- README + Railway template document the two env vars.
