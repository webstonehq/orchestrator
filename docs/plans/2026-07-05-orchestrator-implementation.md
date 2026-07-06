# Orchestrator v1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development — one fresh subagent per task, review between tasks. Every subagent MUST read `docs/plans/2026-07-05-orchestrator-design.md` and the **Shared Contracts** section below before writing code. NO git operations (user rule): tasks end with verification commands, never commits.

**Goal:** Ship Orchestrator v1 — a single Rust binary (API + scheduler + executor + embedded single-file SvelteKit UI) implementing the approved design.

**Architecture:** Rust binary crate at repo root (axum/tokio/rusqlite), plugin trait for task types with declarative UI manifests, in-process sequential executor with parallel fan-out, cron scheduler with catch-up. SvelteKit (Svelte 5, hash router, `bundleStrategy: 'inline'`) builds to one `index.html`, embedded via `include_str!` in release and read from disk in debug.

**Tech Stack:** Rust: axum 0.8, tokio, reqwest (json + rustls), rusqlite (bundled), r2d2_sqlite, croner, chrono + chrono-tz, chacha20poly1305, clap (derive), serde/serde_json/serde_yaml_ng (maintained fork; import path `serde_yaml_ng::` — decided in A1), tracing + tracing-subscriber, async-trait, futures, wiremock + tempfile + tower (dev-deps). UI: SvelteKit 2 + Svelte 5, @sveltejs/adapter-static, @fontsource/ibm-plex-sans + @fontsource/ibm-plex-mono, vitest.

**Working directory:** `/Users/m/projects/github/webstonehq/orchestrator`. Design mock reference (styles/layout ground truth): scratchpad copy of `Civic Lens Orchestrator.dc.html` — visual specs are restated inline in UI tasks, do not depend on the scratchpad surviving.

---

## Shared Contracts (canonical — all tasks conform to these)

### Crate layout (decided during Phase B)
`src/lib.rs` exposes all modules (`pub mod api/config/db/engine/expr/model/plugins/
scheduler/secrets/ui`); `src/main.rs` is a thin CLI shell using `orchestrator::…`.
Integration tests import `use orchestrator::<module>;` — never `#[path]` includes.
No `#![allow(dead_code)]` needed: lib items are public API.

### SQLite schema (migration 001)

```sql
CREATE TABLE flows (
  id TEXT PRIMARY KEY,              -- slug, e.g. "council-alert-pipeline"
  name TEXT NOT NULL,
  namespace TEXT NOT NULL DEFAULT 'default',
  description TEXT NOT NULL DEFAULT '',
  definition TEXT NOT NULL,         -- JSON (FlowDefinition)
  current_rev INTEGER NOT NULL DEFAULT 1,
  paused INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,         -- RFC3339 UTC, everywhere below too
  updated_at TEXT NOT NULL
);
CREATE TABLE flow_revisions (
  flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
  rev INTEGER NOT NULL,
  definition TEXT NOT NULL,
  message TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  PRIMARY KEY (flow_id, rev)
);
CREATE TABLE runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  flow_id TEXT NOT NULL,
  flow_rev INTEGER NOT NULL,
  status TEXT NOT NULL,             -- queued|running|success|failed|canceled
  trigger TEXT NOT NULL,            -- schedule|manual|api
  inputs TEXT NOT NULL,             -- JSON object
  scheduled_for TEXT,               -- cron occurrence covered (catch-up runs)
  started_at TEXT, finished_at TEXT,
  error TEXT
);
CREATE INDEX idx_runs_flow ON runs(flow_id, id DESC);
CREATE INDEX idx_runs_status ON runs(status);
CREATE TABLE task_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL,
  status TEXT NOT NULL,             -- pending|running|success|failed|canceled|skipped
  attempt INTEGER NOT NULL DEFAULT 0,
  result TEXT,                      -- JSON, secret-redacted
  outputs TEXT,                     -- JSON object of extracted outputs
  error TEXT,
  started_at TEXT, finished_at TEXT,
  UNIQUE (run_id, task_id)
);
CREATE TABLE task_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_run_id INTEGER NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  item TEXT NOT NULL,               -- JSON of the fan-out element, redacted
  status TEXT NOT NULL,             -- queued|running|success|failed|canceled|dropped
  attempt INTEGER NOT NULL DEFAULT 0,
  result TEXT, error TEXT,
  started_at TEXT, finished_at TEXT,
  UNIQUE (task_run_id, idx)
);
CREATE INDEX idx_items_status ON task_run_items(task_run_id, status);
CREATE TABLE logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL,
  ts TEXT NOT NULL,
  level TEXT NOT NULL,              -- INFO|OK|WARN|ERR|DBG
  task TEXT NOT NULL DEFAULT 'flow',
  message TEXT NOT NULL
);
CREATE INDEX idx_logs_run ON logs(run_id, id);
CREATE TABLE schedule_state (
  flow_id TEXT NOT NULL,
  trigger_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  next_fire_at TEXT,
  last_fired_at TEXT,
  PRIMARY KEY (flow_id, trigger_id)
);
CREATE TABLE secrets (
  name TEXT PRIMARY KEY,
  ciphertext BLOB NOT NULL,         -- 12-byte nonce || chacha20poly1305 ct
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### FlowDefinition JSON (serde in `src/model/flow.rs`; YAML = same shape + top-level `id`)

```jsonc
{
  "name": "council-alert-pipeline",
  "namespace": "default",
  "description": "",
  "inputs": [ { "id": "provinces", "type": "ARRAY", "required": true, "default": "[\"ON\",\"QC\"]" } ],
  // input types: STRING|ARRAY|DATE|INT|BOOLEAN|JSON. default: template string,
  // parsed per type at run trigger (ARRAY/JSON defaults are JSON text).
  "variables": [ { "id": "server", "value": "https://api.example.com" } ],
  "triggers": [ { "id": "nightly", "type": "schedule", "cron": "0 3 * * *",
                  "timezone": "America/Toronto", "catchup": "latest", "enabled": true } ],
  "tasks": [
    { "id": "discover", "type": "http.request",
      "retry": { "type": "exponential", "max_attempts": 3, "base_seconds": 5 },  // optional; default = 1 attempt
      "timeout_seconds": 60,          // optional, default 60
      "on_error": "fail",             // fail|continue
      "config": { "method": "GET", "url": "{{ vars.server }}/api/municipalities",
                  "headers": [], "body": [], "raw_body": null, "success_codes": "2xx" },
      "outputs": [ { "name": "ids", "type": "ARRAY", "extract": "result.body.ids" } ] },
    { "id": "fetch_all", "type": "parallel",
      "items": "{{ outputs.discover.ids }}", "concurrency": 8,
      "tasks": [ { "id": "fetch_one", "type": "http.request", "on_error": "continue",
                   "config": { "method": "GET", "url": "{{ vars.server }}/api/m/{{ taskrun.value }}" },
                   "outputs": [] } ],
      "outputs": [ { "name": "results", "type": "ARRAY", "extract": "result.items" } ] }
  ]
}
```

### Plugin trait (`src/plugins/mod.rs`)

```rust
pub struct TaskContext { pub run_id: i64, pub task_id: String,
    pub cancel: tokio_util::sync::CancellationToken, /* logger: private */ }
// Construct: TaskContext::new(run_id, task_id, cancel, logger: Box<dyn Fn(LogLevel, String)+Send+Sync>)
// Log via methods: ctx.log(level, msg) / ctx.info/ok/warn/err/dbg(msg).
// Canceled-ness is determined by the ENGINE consulting the token after execute
// returns; a plugin's cancellation error message is display-only ("canceled").

#[async_trait::async_trait]
pub trait TaskPlugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;
    fn validate(&self, config: &serde_json::Value) -> Vec<String>;
    async fn execute(&self, ctx: &TaskContext, config: serde_json::Value)
        -> Result<serde_json::Value, TaskError>;   // TaskError { message, retryable: bool }
}
pub struct PluginRegistry { /* HashMap<String, Arc<dyn TaskPlugin>> */ }
```

`PluginManifest { type_id, label, description, icon, color, fields: Vec<FieldSpec> }`,
`FieldSpec { key, label, widget, required, default: serde_json::Value, help,
options: Option<Vec<String>>, min/max: Option<f64>, template: bool }`.
Widgets: `select|template|keyvalue|number|duration|toggle|text|code` (design doc §2 table).
`parallel` is NOT a plugin — core executor handles it; the UI treats it specially.

### Expression engine (`src/expr/`)

- `parse(template: &str) -> Vec<Segment>` where `Segment = Text(String) | Ref(RefExpr)`;
  `RefExpr { path: String, filters: Vec<Filter> }`. `{{ inputs.since | dateAdd(-7, 'DAYS') }}`.
- `render(template, ctx: &serde_json::Value) -> Result<serde_json::Value, ExprError>`:
  single-Ref template → the referenced JSON value with type preserved; mixed → string
  (JSON values stringified compactly). Unknown ref → error naming the path.
- Context object: `{ "inputs": {...}, "vars": {...}, "outputs": { task: {name: val} },
  "taskrun": { "value": ... }, "secrets": {...} }`. `now()` is the path `now()` resolved
  to current UTC RFC3339; filter `dateAdd(n, 'DAYS'|'HOURS'|'MINUTES')`.
- `render_config(config: &Value, ctx) -> Value`: deep-walk strings in a plugin config.
- Redaction: `redact(value: &mut Value, secret_values: &[String])` replaces any occurrence
  of a secret string in any string value with `***`.

### API routes (axum, `/api` nest; errors: `{ "error": "msg" }` + status code)

```
GET  /api/plugins                       -> [PluginManifest]
GET  /api/dashboard                     -> { active_flows, runs_24h: {total, ok, failed, running},
                                             success_rate_30d, avg_duration_sec, next_scheduled: {flow_id, at} | null }
GET  /api/flows                         -> [ FlowSummary { id, name, namespace, paused, schedule_human,
                                             last_run: {status, finished_at} | null, success_rate_30d, avg_duration_sec, current_rev } ]
POST /api/flows                         { id?, definition } -> flow (id slugified from name if absent)
GET  /api/flows/:id                     -> { id, definition, current_rev, paused, updated_at }
PUT  /api/flows/:id                     { definition, message? } -> { current_rev }   (validates first; 422 on errors)
DELETE /api/flows/:id
POST /api/flows/:id/pause               { paused: bool }
GET  /api/flows/:id/revisions           -> [ { rev, message, created_at } ]
GET  /api/flows/:id/revisions/:rev      -> { definition }
POST /api/flows/validate                { definition } -> { errors: [ { path, message } ] }
GET  /api/flows/:id/export              -> text/yaml
POST /api/flows/import                  text/yaml -> flow (upsert by id)
POST /api/flows/:id/run                 { inputs: {..}, trigger?: "manual" } -> { run_id }  (409 if paused)
GET  /api/runs?flow=&status=&page=&per= -> { runs: [RunSummary], total, counts: {all,running,success,failed,queued,canceled} }
GET  /api/runs/:id                      -> { run, tasks: [TaskRunView], fanout: { task_id: ItemAgg } }
                                           ItemAgg { total, queued, running, success, failed, dropped, retried }
POST /api/runs/:id/cancel
POST /api/runs/:id/replay               -> { run_id }
GET  /api/runs/:id/logs?after_id=       -> { logs: [ {id, ts, level, task, message} ] }
GET  /api/runs/:id/events               -> SSE (below)
GET  /api/runs/:id/tasks/:task/items?status=&page=&per= -> { items: [...], total }
POST /api/runs/:id/tasks/:task/retry-failed -> { run_id }   (new run; inputs = { items: [failed item values] } convention documented in UI)
GET  /api/schedules                     -> [ { flow_id, flow_name, trigger_id, cron, timezone, human, catchup,
                                             enabled, next_fire_at, last_fired_at, last_run_status } ]
POST /api/schedules/:flow/:trigger/toggle { enabled: bool }
GET  /api/secrets                       -> [ { name, created_at, updated_at } ]
PUT  /api/secrets/:name                 { value }        DELETE /api/secrets/:name
```

### SSE events (`/api/runs/:id/events`)

Named events, JSON data. On connect: `snapshot` = full `GET /api/runs/:id` payload +
`last_log_id`. Then: `run {status, finished_at?, error?}` · `task {task_id, status,
attempt}` · `items {task_id, ...ItemAgg, throughput_per_sec}` (throttle ≥500ms) ·
`log {id, ts, level, task, message}`. Heartbeat comment every 15s.

### Engine internals

`Engine` owns: db pool, `PluginRegistry`, `SecretStore`, map `run_id -> CancellationToken`,
and a `tokio::sync::broadcast` per active run for SSE fan-out (events enum mirrors SSE).
`Engine::start_run(run_id)` spawns a task: load flow rev definition → resolve inputs
(defaults rendered, required enforced) → iterate tasks sequentially. Per task: build ctx,
`render_config`, execute with `tokio::time::timeout` + `tokio::select!` on cancel token;
on TaskError retryable && attempts left → sleep `base_seconds * 2^(attempt-1)` then retry.
Extract declared outputs (dotted path with `[n]` indices into result; missing path ⇒ task
failure). Parallel task: render `items` (must be array), insert item rows, run
`futures::stream::iter(..).map(|item| child_chain(item)).buffer_unordered(concurrency)`;
child chain = its tasks sequentially with `taskrun.value` in ctx and child outputs
accumulated per item; final item result = last child's result; task result =
`{ "items": [ per-item final results in idx order, dropped items -> null ] }`.
`on_error: continue` in a child ⇒ item status `dropped`, WARN log, null result.
Secrets: resolved into ctx lazily on first `secrets.` ref; all stored results/items/logs
pass through `redact` with all resolved secret values. On startup:
`UPDATE runs SET status='failed', error='interrupted by shutdown' WHERE status IN ('queued','running')`
(and same for task_runs/items).

### Scheduler

Loop every 1s (plus `Notify` poke on schedule mutation): for each enabled schedule with
`next_fire_at <= now`: catch-up policy from trigger (`none|latest|all`, default `latest`):
missed occurrences = all fire times in `(next_fire_at ..= now]`. none ⇒ fire nothing,
advance. latest ⇒ queue 1 run (`scheduled_for` = most recent missed). all ⇒ queue one per
missed, cap 100 + WARN. Then set `next_fire_at` = next occurrence strictly after now
(croner, in trigger tz). On flow save/import: reconcile `schedule_state` rows (insert new
w/ computed next_fire_at, delete removed, keep enabled flag on survivors). Paused flows:
scheduler skips (logs DBG).

### UI conventions

Svelte 5 runes; NO component library. `ui/src/lib/api.ts` typed fetch client;
`ui/src/lib/theme.css` defines the mock's tokens: `--bg:#0a0c10 --bg2:#0d1015
--panel:#12161d --panel2:#161b23 --panel3:#1b212b --border:#222a35 --border2:#2c3643
--text:#e7edf5 --muted:#8a95a6 --dim:#5a6675 --accent:#7ee787 --green:#3fb950
--cyan:#58a6ff --red:#f85149 --amber:#e3b341`. Fonts: IBM Plex Sans (400–700),
IBM Plex Mono (400–600) via @fontsource, latin subset only. Status pill/dot styles per
mock (`pill()`/`dot()` in the mock script). Routes (hash): `#/` flows dashboard ·
`#/flows/new` · `#/flows/:id` builder/editor · `#/runs` · `#/runs/:id` · `#/schedules`
· `#/secrets`. Template⇄chip parsing lives in `ui/src/lib/template.ts` (vitest-covered);
it must round-trip exactly with the Rust parser's grammar.

**Routing (changed 2026-07-06 by Mike's directive):** NO hash router. Default
pathname routing + SvelteKit SPA mode (`adapter-static` with `fallback:
'index.html'`, `ssr = false`); the Rust server serves index.html for all
non-API paths, so deep links work. Links are plain paths (`/flows/x`),
query params via `page.url.searchParams`. Routes: `/` flows dashboard ·
`/flows/new` · `/flows/:id` · `/runs` · `/runs/:id` · `/schedules` · `/secrets`.

---

## Phase A — Scaffolding

### Task A1: Rust crate scaffold
**Files:** `Cargo.toml`, `src/main.rs`, `src/config.rs`, `.gitignore`, `rust-toolchain.toml` (stable)
- clap derive: `orchestrator serve [--listen 127.0.0.1:4400] [--db PATH] [--key PATH]`;
  defaults `~/.orchestrator/{orchestrator.db,master.key}` (dirs crate), create dir 0700.
- `serve` for now: init tracing, print config, bind a placeholder axum router with
  `GET /api/health -> {"ok":true}`.
- All deps from Tech Stack pinned by major version.
**Verify:** `cargo build` clean (no warnings with `cargo clippy -- -D warnings`);
`cargo run -- serve` then `curl localhost:4400/api/health` returns `{"ok":true}`.

### Task A2: SvelteKit single-file scaffold + embedding
**Files:** `ui/` (create-svelte skeleton, TS), `ui/svelte.config.js`, `ui/vite.config.ts`,
`ui/src/routes/+layout.ts` (`ssr=false; prerender=true` as adapter-static requires),
`ui/src/lib/theme.css`, `src/ui.rs`, modify `src/main.rs`.
- svelte.config: adapter-static (`fallback: 'index.html'` not needed with hash router —
  use `pages/assets: 'build'`), `kit.output.bundleStrategy: 'inline'`,
  `kit.router.type: 'hash'`. Confirm `ui/build/index.html` is the ONLY emitted file
  (plus maybe favicon — inline it via `%sveltekit.assets%`-free approach: put favicon
  as data URI in app.html).
- Placeholder root page rendering "Orchestrator" with theme tokens + both fonts.
- `src/ui.rs`: `#[cfg(debug_assertions)]` read `ui/build/index.html` from disk per
  request (falling back to a "UI not built — run npm run build in ui/" page);
  release: `include_str!`. Route: GET `/` and any non-`/api` path → the HTML.
- `ui/vite.config.ts`: dev server proxy `/api` → `http://127.0.0.1:4400`.
- `ui/package.json` scripts: `dev`, `build`, `test` (vitest).
**Verify:** `cd ui && npm install && npm run build` → single `build/index.html`
(`ls ui/build` shows only index.html + favicon assets inlined); `grep -c "<script" ui/build/index.html` ≥1 with no external `src=`/`href=` asset references (`grep -E '(src|href)="/_app' ui/build/index.html` → no matches). `cargo run -- serve` serves the page at `/`.

## Phase B — Core Rust modules (B1–B5 parallelizable after A1)

### Task B1: DB layer
**Files:** `src/db.rs`, `tests/db.rs`
- r2d2 pool, WAL + foreign_keys pragmas, embedded migration runner (`migrations` table,
  001 = Shared Contracts schema).
- Typed row structs + CRUD helpers used later (flows upsert+revision in one tx,
  runs/task_runs/items/logs inserts & queries incl. item aggregates + status filters,
  schedule_state reconcile, dashboard metric queries).
**Tests:** migration idempotency; flow save bumps rev + revision row; item aggregate
counts; dashboard metrics on seeded data.
**Verify:** `cargo test db` green; clippy clean.

### Task B2: Expression engine
**Files:** `src/expr/mod.rs`, `src/expr/parse.rs`, `src/expr/render.rs`
**Tests (in-module):** parse round-trip (`to_string` == input); text-only; single-ref
type preservation (array stays array); mixed stringification; nested paths + `[0]`
indexing; `now()` shape; `dateAdd` day/hour/minute (+negative); unknown ref error names
path; unclosed `{{` error; `render_config` deep walk; `redact` replaces embedded secret
substrings.
**Verify:** `cargo test expr` green.

### Task B3: Secrets store
**Files:** `src/secrets.rs`
- Keyfile: 32 bytes, created 0600 if missing; chacha20poly1305, random 12-byte nonce
  prepended to ciphertext. `set/get/list/delete`; `resolve_all() -> HashMap<String,String>`
  for the engine.
**Tests:** round-trip; wrong key fails; list returns no values; file perms 0600.
**Verify:** `cargo test secrets` green.

### Task B4: Plugin system + HTTP plugin
**Files:** `src/plugins/mod.rs`, `src/plugins/http.rs`
- Trait/registry/manifest per Shared Contracts. HTTP manifest fields: method (select),
  url (template, required), headers (keyvalue), body (keyvalue), raw_body (code,
  help "overrides body params"), success_codes (text, default "2xx" — grammar:
  comma list of codes/classes e.g. `2xx,301`).
- execute: reqwest with per-call client, no timeout here (engine owns timeout);
  status match → Ok `{status, headers, body}` (json-parse body else string);
  non-match → TaskError retryable (5xx/timeout/connect) or not (4xx).
**Tests (wiremock):** GET json body; POST body params become JSON object; raw_body wins;
headers sent; 500 → retryable error; 404 → non-retryable; success_codes "404" accepts 404;
non-JSON body returned as string.
**Verify:** `cargo test plugins` green.

### Task B5: Flow model + validation + YAML
**Files:** `src/model/mod.rs`, `src/model/flow.rs`, `src/model/validate.rs`
- Serde types per Shared Contracts (deny_unknown_fields on the definition).
- `validate(def, &PluginRegistry) -> Vec<ValidationErr { path, message }>`: unique
  task/input/var ids; slug id rules; cron parses (croner) + tz known; every `{{ ref }}`
  in any template resolves to inputs/vars/secrets/functions or **upstream** task outputs
  (parallel children may also use `taskrun.*` and prior-sibling outputs); plugin type
  exists; plugin.validate(config) merged in; outputs extract paths start `result`;
  parallel: items present, concurrency 1–256, ≥1 child, children are plugin tasks.
- YAML export (`id` injected at top) / import (parse → validate).
**Tests:** happy path from design-doc example; each validator class (dup id, bad cron,
forward ref, unknown plugin, bad extract root, child ref to later sibling); YAML
round-trip equality.
**Verify:** `cargo test model` green.

## Phase C — Engine & scheduler (after B*)

### Task C1: Run engine
**Files:** `src/engine/mod.rs`, `src/engine/run.rs`, `src/engine/events.rs`, `tests/engine.rs`
Per Shared Contracts "Engine internals". Public surface:
`Engine::new(pool, registry, secrets) -> Arc<Engine>`; `create_run(flow_id, inputs,
trigger, scheduled_for) -> run_id` (validates+defaults inputs); `start(run_id)`;
`cancel(run_id)`; `subscribe(run_id) -> broadcast::Receiver<RunEvent>`; startup recovery fn.
**Tests (wiremock end-to-end):** two-task output chaining; extraction failure fails task;
retry: 500,500,200 with backoff (tokio test time pause) → success attempt 3; timeout →
retryable; fan-out 10 items conc 3 (assert ≤3 concurrent via wiremock delay + counter,
all item rows success, result.items ordered); on_error continue → dropped item, null in
result.items, task succeeds; child chain: second child sees first child output +
taskrun.value; cancel mid-fan-out → run canceled, no new items start; secret redaction:
secret value absent from stored results/logs (grep DB); interrupted-run recovery.
**Verify:** `cargo test engine` green.

### Task C2: Scheduler
**Files:** `src/scheduler.rs`
Per Shared Contracts "Scheduler". Inject a `Clock` trait (real + mock) so tests don't sleep.
**Tests:** next-fire respects timezone (croner + America/Toronto DST boundary case);
catch-up none/latest/all with process "off" gap (mock clock jump); `all` cap 100; toggle
disable→enable recomputes from now; paused flow skipped; reconcile on definition change.
**Verify:** `cargo test scheduler` green.

## Phase D — API (after C; D1/D2 parallelizable)

### Task D1: Flows/plugins/secrets/dashboard/schedules routes
**Files:** `src/api/mod.rs`, `src/api/flows.rs`, `src/api/misc.rs`, `tests/api_flows.rs`
Routes per Shared Contracts (all except runs/SSE). AppState { pool, engine, registry,
secrets, scheduler_notify }. PUT validates → 422 w/ errors list. Export sets
`content-type: text/yaml`. Secrets PUT upserts; GET never returns values.
**Tests (axum + tempdb):** flow CRUD + revision bump + revision fetch; validate endpoint
mirrors model errors; import/export round-trip; pause blocks `POST /run` (409);
dashboard shape on seeded data; schedule toggle persists + pokes notify; secrets
lifecycle + name-only listing.
**Verify:** `cargo test api_flows` green.

### Task D2: Runs routes + SSE
**Files:** `src/api/runs.rs`, `tests/api_runs.rs`
Routes per Shared Contracts. SSE bridges `engine.subscribe` broadcast → axum SSE with
snapshot-first semantics and 15s keepalive; logs endpoint pages by `after_id`;
items endpoint filters/paginates; retry-failed builds new run with
`inputs = { "items": [failed item values] }`.
**Tests:** trigger run via API against wiremock flow → poll to success; run list filters
+ counts; cancel endpoint; items pagination + status filter; SSE: connect during run,
assert snapshot then ≥1 task event then run success event; retry-failed creates run with
failed items as inputs.
**Verify:** `cargo test api_runs` green.

### Task D3: Wire `serve`
**Files:** modify `src/main.rs`
Startup order: config → db+migrate → secrets → registry (http) → engine (+recovery) →
scheduler spawn → axum (api nest + ui fallback) → graceful shutdown (ctrl-c: cancel
running runs as 'interrupted', stop scheduler).
**Verify:** `cargo run -- serve` with a hand-made flow via curl: create flow (POST),
run it against `https://httpbingo.org/json` alternative — use local `python3 -m http.server`
serving a JSON fixture instead (offline-safe); watch SSE with `curl -N`; confirm logs,
task states, dashboard numbers. Clippy clean across workspace.

## Phase E — UI foundation (after A2; E1/E2 parallelizable with Phase C/D)

### Task E1: Shell, routing, API client
**Files:** `ui/src/lib/api.ts`, `ui/src/lib/theme.css` (finalize), `ui/src/routes/+layout.svelte`
(sidebar: logo block, Workspace nav [Flows/Runs/Schedules/Secrets], footer engine widget
"orchestrator · N running" from dashboard poll; header: breadcrumb slot, avatar dropped),
`ui/src/lib/components/{StatusPill,StatusDot,MetricCard,Toggle,Modal}.svelte`,
route stubs for all hash routes.
Visuals: 238px sidebar `--bg2` right-border `--border`; nav item active state =
`--panel2` bg + inset 2px accent bar; header 56px `--bg2` mono breadcrumb — per mock.
**Verify:** `npm run build` single file; manual: navigation works on all stubs, pills
render all six statuses.

### Task E2: Template chips + schema-driven forms
**Files:** `ui/src/lib/template.ts`, `ui/src/lib/template.test.ts`,
`ui/src/lib/components/fields/{FieldRenderer,TemplateInput,KeyValueRows,SelectChip,
NumberStepper,DurationInput,ToggleField,TextField,CodeField}.svelte`,
`ui/src/lib/components/ExprPicker.svelte`
- `template.ts`: `parse(tpl) -> Token[{kind:'text'|'ref', value}]`, `serialize(tokens)`;
  grammar identical to Rust `expr` (document divergences = bugs).
- `TemplateInput`: ref chips (blue, deletable) + literal text + `{ } insert` button
  opening `ExprPicker`: grouped INPUTS / OUTPUTS·task / VARIABLES / SECRETS / FUNCTIONS
  (/ ITERATION + PRIOR STEPS inside parallel children) — groups supplied by caller.
- `FieldRenderer`: switch on `FieldSpec.widget`, emits `config[key]` updates.
**Tests:** vitest — parse/serialize round-trips, mixed templates, nested paths, adjacent
refs; picker grouping helper given a def + task index returns correct upstream-only refs.
**Verify:** `npm run test` green; `npm run build` still single-file.

## Phase F — UI screens (after E2 + D*; F1–F4 parallelizable)

### Task F1: Dashboard + Runs + Schedules screens
**Files:** `ui/src/routes/+page.svelte` (dashboard), `ui/src/routes/runs/+page.svelte`,
`ui/src/routes/schedules/+page.svelte`
Mock-faithful: 5 metric cards grid; flows table (grid `2.4fr 1.2fr 1.1fr 1fr 0.9fr`,
status dot+name+namespace, schedule human, last-run pill+ago, success bar
green≥90/amber≥80/red, avg duration; row hover `--panel2`; click → `#/flows/:id`).
Runs: filter chips with live counts, table w/ trigger dot (schedule=accent, api=cyan,
manual=gray), 3s poll while any running. Schedules: rows w/ clock icon, human text via
cronstrue? NO — server provides `human`; toggle switch (accent when on) hits toggle API.
Empty states for all three (no flows yet → CTA "New flow").
**Verify:** `npm run build`; manual against seeded server: counts match API, filters
work, toggle round-trips, relative times render ("22m ago").

### Task F2: Flow builder/editor
**Files:** `ui/src/routes/flows/new/+page.svelte`, `ui/src/routes/flows/[id]/+page.svelte`
(both thin wrappers over `ui/src/lib/builder/Builder.svelte` + subcomponents:
`TaskCanvas`, `InspectorInputs`, `InspectorVars`, `InspectorTrigger`, `InspectorTask`
(manifest-driven via FieldRenderer + envelope controls: retry attempts/base, timeout,
on_error, outputs table w/ name/type/extract), `InspectorParallel` (items TemplateInput,
concurrency stepper, child list where each child = InspectorTask minus outputs? NO —
children keep outputs; child picker gets ITERATION + PRIOR STEPS groups), `YamlPane`
(client-side YAML render of the definition + syntax highlight matching mock colors:
keys #79c0ff, strings/templates #a5d6ff, numbers/keywords #f69d50, comments #5f7566,
punctuation #566270), `RevisionsPanel`, `RunModal`).
Behavior: editable name/namespace/description header; type pills (Inputs n / Variables n
/ Trigger n) + task nodes on canvas (fan-out card w/ dashed bar per mock); debounced
`POST /flows/validate` → ready/incomplete badge gating Save & Run; Save → PUT w/
optional message prompt → toast rev N; revisions panel: list, view (readonly load),
restore (loads def into editor, save creates new rev); Run → RunModal generated from
declared inputs (typed controls: STRING/DATE text, INT number, BOOLEAN toggle,
ARRAY/JSON textarea w/ JSON validation; required enforcement; defaults prefilled) →
POST run → navigate `#/runs/:id`. New-task menu = plugin manifests (+ Parallel).
Trigger inspector: cron text input w/ live human preview (`/api/flows/validate` returns
cron errors; human string computed server-side on save — client shows raw + tz select
from a static IANA list + catchup select + enabled toggle). YAML export = download link;
import = paste-YAML modal on dashboard "New flow" split-button.
**Verify:** `npm run build`; manual: create the design-doc example flow end-to-end via
UI only, save, edit, restore old rev, trigger run with modal.

### Task F3: Run detail + fan-out inspector
**Files:** `ui/src/routes/runs/[id]/+page.svelte`, `ui/src/lib/run/{ExecGraph,LogsPane,
TimelinePane,FanoutModal}.svelte`
- SSE via EventSource (hash-router-safe URL), reconnect w/ backoff, fall back to 3s
  polling if SSE errors twice.
- ExecGraph: vertical chain from definition order (SVG bezier edges, 220px nodes,
  fan-out card 300px w/ live seg bar from `items` events; node states per mock:
  running = nodePulse animation border cyan, success = green inset, pending = 0.72
  opacity). Edge data labels: `outputs` name + count when producing task finished
  (array length badge).
- LogsPane: mono grid ts/level/task/msg, level colors {INFO cyan, OK green, WARN amber,
  ERR red, DBG gray}, autoscroll w/ stick-to-bottom toggle, live via SSE `log` events
  seeded by `GET logs`.
- TimelinePane: Gantt from task started/finished vs run span, real axis ticks.
- FanoutModal: stats tiles (total/completed/in-flight/failed/retried/throughput),
  progress bar segments, ETA from throughput, in-flight list (running items w/ elapsed),
  canvas heatmap: cell grid colored by item status batches from
  `GET items?per=all-statuses` summary — paint from paged fetches of statuses only
  (endpoint returns `statuses: "sffd..."` compact string when `?format=heatmap`; ADD
  this format to D2 endpoint), Failed/Slow tabs from item queries (slow = success,
  duration desc, top 20), Retry failed → POST retry-failed → toast w/ link to new run.
- Header: status pill, elapsed ticker (client clock from started_at), Cancel (confirm),
  Replay.
**Verify:** `npm run build`; manual: watch a live fan-out run (seed script from G1),
logs stream, cancel works, heatmap renders 1000+ items smoothly.

### Task F4: Secrets screen + empty/error polish
**Files:** `ui/src/routes/secrets/+page.svelte`, sweep pass over all screens
Secrets: table name/created/updated, add form (name + value, value never re-shown),
update = same PUT, delete w/ confirm. Reference hint: `{{ secrets.NAME }}`.
Sweep: consistent empty states, API error toasts, loading skeletons, focus states,
`prefers-reduced-motion` guard on pulse animations.
**Verify:** `npm run build`; manual pass on every screen.

## Phase G — Integration

### Task G1: Demo seed + end-to-end verification + README
**Files:** `scripts/demo.sh` (bash: starts a tiny python3 mock API on :4599 serving
fixture JSON endpoints incl. a slow paginated one; curls Orchestrator API to import
`examples/demo-flow.yaml` — a 3-task flow w/ fan-out over the mock), `examples/demo-flow.yaml`,
`README.md` (what it is, single-binary build: `cd ui && npm i && npm run build && cd .. &&
cargo build --release`, quickstart, flow YAML reference, plugin-authoring guide w/ the
trait + manifest walk-through, security note re listen addr), `LICENSE` (MIT).
**Verify (full system):** release build embeds UI (`cargo build --release` after ui
build; run binary from an empty temp dir — UI loads, no ui/ dir needed); demo flow runs
green end-to-end watched live in the run screen; `cargo test` full suite green;
`cargo clippy --all-targets -- -D warnings` clean; `cd ui && npm run test` green.

---

## Execution notes for the orchestrating session

- Subagent per task (general-purpose), model: inherit session model for all tasks —
  builder (F2), engine (C1) and run-detail (F3) are the hardest; do NOT downgrade those.
  A1/A2/B3/F4/G1 may run on sonnet if budget demands.
- Order: A1 → A2 → {B1..B5 parallel} → {C1, C2 parallel} → {D1, D2 parallel} → D3 →
  {E1, E2} (may overlap with C/D) → {F1..F4 parallel} → G1.
- After each phase: orchestrator reviews diffs (superpowers:requesting-code-review
  criteria), runs the phase's verify commands itself before dispatching the next phase.
- Contract drift rule: if a subagent must deviate from Shared Contracts, it reports the
  deviation in its final message; the orchestrator updates this doc before dependent
  tasks dispatch.

## Post-v1 notes (accepted limitations, discovered in review)
- `TaskPlugin::validate` returns flat `Vec<String>`, so plugin config errors anchor at
  `tasks[i].config` while template errors anchor at `tasks[i].config.<field>`. Future:
  evolve the trait to `(field_path, message)` pairs. Meanwhile plugins should prefix
  messages with the field name ("url is required").
- YAML import: duplicate keys are last-wins (serde_yaml_ng behavior), not rejected.
- HTTP plugin buffers response bodies fully (no size cap); Set-Cookie multi-value
  joining is lossy.
