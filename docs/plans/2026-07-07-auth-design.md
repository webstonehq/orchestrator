# Authentication — Design

**Date:** 2026-07-07
**Status:** Implemented
**Motivation:** Orchestrator now ships as a Railway template with a public URL. The
human-facing UI and JSON API are currently unauthenticated. This adds a login
gate so a public deployment is safe for a single operator, with room to grow to
multiple users later.

> **Revision (2026-07-07):** the admin is now created via a **first-run
> onboarding screen**, not seeded from environment variables. The
> env-seeding sections below are superseded — see
> *Credential source — first-run onboarding*.

## Scope

**In scope (v1):**

- Username + password accounts stored in SQLite, argon2id-hashed.
- First admin created through a one-time onboarding screen (no credential env
  vars); setup closes once any account exists.
- Server-side sessions (opaque token in a `sessions` table), delivered as an
  HTTP-only cookie.
- Login / logout / "who am I" / setup endpoints.
- A middleware guard over all human data endpoints.
- A layout-level auth gate in the SvelteKit SPA with onboarding, login, and
  logout.

**Out of scope (deliberately — YAGNI for a currently single-user app):**

- In-app password change / reset (recover by clearing the `users` row and
  re-running setup).
- User management UI (add/remove/list users, roles, invites).
- Sliding session renewal, background session sweeper, rate limiting beyond
  argon2's inherent cost.
- Reverse-proxy / OAuth / external identity delegation.

The worker control-plane API (`/api/worker/*`) already authenticates with its
own bearer tokens (machine auth). That scheme is **unchanged and independent**:
a human session never grants worker access, and a worker token never grants
human access.

## Data model

A new migration (version `5`, appended to `MIGRATIONS` in `src/db.rs`) creates:

```sql
CREATE TABLE users (
  id         INTEGER PRIMARY KEY,
  username   TEXT NOT NULL UNIQUE,
  pw_hash    TEXT NOT NULL,          -- argon2id PHC string
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE sessions (
  token      TEXT PRIMARY KEY,       -- 256-bit random, base64url
  user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
```

New dependency: `argon2 = "0.5"` (pure Rust, no C toolchain needed).

## Credential source — first-run onboarding

There are no credential env vars. The admin is created through the UI on first
run:

- On startup nothing is provisioned. A fresh database simply has an empty
  `users` table.
- `GET /api/auth/setup` reports whether onboarding is still open
  (`{"needed": <users table empty>}`). The SPA calls it when unauthenticated to
  choose the onboarding screen vs. the login screen.
- `POST /api/auth/setup {username, password}` creates the first admin **only
  while the table is empty**, then logs them in (issues a session cookie).
  `Db::create_first_user` runs in an `IMMEDIATE` transaction and re-checks
  emptiness inside it, so two concurrent setups can't both win. Once any account
  exists the endpoint returns `409` — the setup window is closed for good.

Rationale and consequences:

- **No pre-configuration.** Works identically for a bare binary, Docker, or the
  Railway template — deploy, open the URL, create the account. No env to set.
- **First-visitor race.** On a public URL the setup screen is claimable by
  whoever loads it first, so setup must be completed immediately after exposing
  the domain (documented in the README and `docs/railway.md`). The `409` guard
  ensures it can only ever be claimed once.
- **No rotation/reset path.** Since there is no in-app password change and no env
  override, recovering a lost password means clearing the account (delete the
  `users` row, which cascades to `sessions`, or start against a fresh `--db`);
  the next load re-opens onboarding.

## Request flow, endpoints & the guard

### Public endpoints (exempt from the guard)

Added under `/api/auth` in `api::router`:

- `POST /api/auth/login` — body `{username, password}`. Verify the argon2 hash.
  On success: insert a session row, reply `200 {username}` with
  `Set-Cookie: orch_session=<token>; HttpOnly; SameSite=Lax; Path=/; Max-Age=…`
  (plus `Secure` conditionally — see below). On failure: `401 {"error":"invalid
  credentials"}`. A missing user still runs a dummy argon2 verify so response
  timing does not reveal whether a username exists.
- `POST /api/auth/logout` — delete the session row for the cookie's token, clear
  the cookie. Idempotent (`200` even with no active session).
- `GET /api/auth/me` — `200 {username}` if the cookie maps to a live, unexpired
  session; otherwise `401`. The SPA uses this to decide app vs. unauthenticated
  on load.
- `GET /api/auth/setup` — `200 {"needed": bool}`; whether first-run onboarding is
  still available (no account exists yet).
- `POST /api/auth/setup` — body `{username, password}`. Creates the first admin
  and logs them in (same session cookie as login). `400` on empty username or a
  password under 8 chars; `409` once any account exists.

Also unguarded, as today:

- `GET /api/health` — Railway's health check must not require a cookie.
- `/api/worker/*` — keeps its own bearer-token auth.
- The UI shell (`/` and the SPA deep-link fallback) — contains no data; the SPA
  gates itself via `/api/auth/me`.

### The guard

An axum middleware (`axum::middleware::from_fn_with_state`) wraps the
flows / misc / runs routers. For each request it:

1. Reads the `orch_session` cookie.
2. Looks up the session row and checks `expires_at`.
3. On success, injects the authenticated user into request extensions and calls
   the inner handler.
4. Otherwise short-circuits with `401 {"error":"unauthorized"}`.

Boundary summary: **all human data endpoints require a session; health, the auth
endpoints, the worker API, and the static shell do not.**

## Cookie, session & security posture

- **Cookie attributes:** `HttpOnly` (blocks JS token theft via XSS),
  `SameSite=Lax` (cookie rides top-level navigations so deep links work, blocks
  cross-site POST CSRF), `Path=/`.
- **`Secure` — conditional.** Set when the request arrived over HTTPS (request
  scheme `https`, or `X-Forwarded-Proto: https`), omitted otherwise. Railway
  terminates TLS and forwards `X-Forwarded-Proto: https`, so the cookie is
  `Secure` there; a bare `http://127.0.0.1` run omits it, where `Secure` would
  otherwise make the browser silently drop the cookie and lock the operator out.
- **Session lifetime:** fixed **30-day** `expires_at` set at login. No sliding
  renewal in v1. Logout deletes the row immediately.
- **Expiry handling:** the guard rejects any session past `expires_at`. A cheap,
  indexed lazy sweep (`DELETE FROM sessions WHERE expires_at < now`) runs
  opportunistically at login — no background task.
- **Token generation:** 32 bytes from the OS CSPRNG, base64url-encoded. Looked
  up by primary key; plain equality is safe because the token is high-entropy
  and unguessable (unlike a password).
- **Brute-force posture:** argon2id's per-attempt cost (~tens of ms) is the
  primary defense against online password guessing for a single-user public
  app; no separate rate limiter in v1.

## UI (SvelteKit SPA)

- **Auth bootstrap:** the root layout calls `GET /api/auth/me` before rendering
  the main UI (state machine `loading → setup | out | in`).
  - `200 {username}` → render the app; stash `username` for a "signed in as … /
    Log out" affordance.
  - `401` → call `GET /api/auth/setup`; `{needed:true}` → **onboarding screen**,
    otherwise the **login view**. A gate at the layout level; no client-side
    route table changes. The global-401 handler is suppressed until the initial
    probe finishes so it can't flash the login screen before the setup check
    resolves.
- **Onboarding view (`Setup.svelte`):** centered card in the dark IBM Plex /
  `#7ee787` theme — username, password, confirm password (client-side: ≥8 chars,
  must match). On submit → `POST /api/auth/setup`; on `200` re-run the bootstrap
  (now authenticated → app); `409` → "setup already completed, reload".
- **Login view (`Login.svelte`):** same theme — username, password, submit. On
  submit → `POST /api/auth/login`; on `200` re-run bootstrap; on `401` show an
  inline "invalid credentials" message.
- **Global 401 handling:** the API `fetch` wrapper fires a registered handler on
  any `401` (after boot), dropping back to the login view for a mid-session
  expiry. Every existing call site is unchanged.
- **Logout:** a "Log out" control in the top bar → `POST /api/auth/logout` →
  login view.
- **Cookies are automatic:** same-origin `fetch` sends `orch_session` with no
  code change. `HttpOnly` means the SPA never reads the token.

The SPA remains a single inlined file served by the Rust binary; these changes
rebuild into `ui/build/index.html` exactly as today (debug reads from disk,
release embeds via `include_str!`).

## Testing

**Rust (integration, `tests/`):**

- Migration applies cleanly on a fresh DB; `users` / `sessions` tables exist.
- `create_first_user` inserts while empty and returns `None` (no overwrite) once
  a user exists; `has_users` reflects state.
- Setup: `GET /setup` needed when empty; `POST /setup` creates + logs in;
  rejects with `409` once configured; `400` on short password / blank username.
- Login success sets a session cookie and inserts a row; `me` with that cookie
  returns the username.
- Login with a wrong password / unknown user returns `401`; unknown user still
  incurs an argon2 verify (no early return).
- Guarded endpoint returns `401` with no cookie, `200` with a valid cookie,
  `401` with an expired/deleted session.
- Logout deletes the row and subsequent `me` returns `401`.
- Health and `/api/worker/*` remain reachable under their existing rules (no
  human cookie required).
- `Secure` present when `X-Forwarded-Proto: https`, absent on plain http.

**UI (`ui/` tests + Playwright E2E):**

- `api.auth.{me,login,logout,setupNeeded,setup}` client shape; global-401 hook
  fires once per 401.
- E2E: fresh DB → onboarding; create account → app; reload persists; logout →
  login (not onboarding); wrong password → error; correct login → app.

## Config / env summary

No credential env vars — the admin is created via first-run onboarding
(`/api/auth/setup`). No new CLI flags. The worker token config (`--worker-token` /
`ORCH_WORKER_TOKENS`) is untouched.

## Open follow-ups (post-v1)

- In-app password change and/or an `orchestrator reset-admin`/`user` CLI, so a
  lost password doesn't require SQLite surgery.
- Multi-user management with roles.
- Login rate limiting if abuse is observed.
