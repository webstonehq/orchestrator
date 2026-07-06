# Orchestrator — v1 Design

**Date:** 2026-07-05
**Status:** Approved (Mike, 2026-07-05)

Orchestrator is a single-binary workflow orchestration tool. Flows are ordered
lists of tasks (HTTP requests in v1, extensible via plugins) with typed inputs,
variables, cron triggers, parallel fan-out, retries, and full run
observability. The UI follows the "Civic Lens Orchestrator" design mock
(claude.ai/design project `80a3c963-ede2-4832-99dc-dcecb3ab16b2`) — dark IBM
Plex terminal aesthetic, green accent `#7ee787`.

Personal tool for Mike, open source: contributor-friendliness is a design
goal, especially for task-type plugins.

## 1. Distribution & repo shape

One Rust binary serves everything: web UI, JSON API, scheduler, executor.
CLI surface in v1 is `orchestrator serve` only (plus `--db`, `--key`,
`--listen` flags); more subcommands later.

```
orchestrator/
├── Cargo.toml              # binary crate
├── src/
│   ├── main.rs             # CLI entry (clap), serve command
│   ├── db.rs               # rusqlite pool, migrations
│   ├── model/              # flow definition, run state types
│   ├── expr/               # expression engine
│   ├── plugins/            # plugin trait + registry + builtin plugins
│   │   ├── mod.rs          # TaskPlugin trait, PluginRegistry, manifest types
│   │   └── http/           # http.request plugin (manifest + execute)
│   ├── engine/             # run executor, fan-out, retries, cancellation
│   ├── scheduler.rs        # cron loop + catch-up
│   ├── secrets.rs          # encrypted secrets store
│   ├── api/                # axum routes, SSE
│   └── ui.rs               # embedded index.html serving
├── ui/                     # SvelteKit app (Svelte 5)
└── docs/plans/
```

**UI build:** SvelteKit + `adapter-static` + `kit.output.bundleStrategy:
'inline'` + SPA fallback (pathname routing; changed from hash routing on
2026-07-06 by Mike's directive) → one self-contained
`ui/build/index.html`. IBM Plex Sans/Mono via `@fontsource` static
weights (latin subset) so fonts inline into the bundle — no network
dependency. Release builds embed via `include_str!`; debug builds read the
file from disk at runtime (UI iteration without recompiling Rust). UI dev:
Vite dev server with `/api` proxied to the Rust process.

**Rust stack:** axum + tokio (HTTP/SSE) · reqwest (outgoing requests) ·
rusqlite w/ WAL + small pool (r2d2_sqlite) · croner + chrono-tz (cron w/
timezones) · chacha20poly1305 (secrets) · clap (CLI) · serde/serde_json ·
serde_yaml_ng (import/export) · tracing (server logs).

**Files at runtime:** `~/.orchestrator/orchestrator.db`,
`~/.orchestrator/master.key` (0600, auto-generated on first run). Both
overridable via flag/env.

## 2. Plugin architecture (task types)

Task types are plugins. A plugin contributes **both** its execution code and
its task-inspector UI, the latter declaratively. Compile-time registration
(no dynamic loading in v1): a contributor adds a module under `src/plugins/`,
implements the trait, registers it in the registry — one place in Rust, zero
frontend changes. The UI renders every task inspector from plugin manifests
fetched at `/api/plugins`.

```rust
#[async_trait]
pub trait TaskPlugin: Send + Sync {
    /// Static metadata + declarative config UI.
    fn manifest(&self) -> PluginManifest;
    /// Validate a task's config JSON (referenced fields, required, shapes).
    fn validate(&self, config: &serde_json::Value) -> Vec<String>;
    /// Execute with fully-rendered config (expressions already resolved).
    /// Returns a JSON result; declared outputs extract from it.
    async fn execute(&self, ctx: &TaskContext, config: serde_json::Value)
        -> Result<serde_json::Value, TaskError>;
}
```

`PluginManifest` (served as JSON): `type_id` ("http.request"), `label`,
`description`, `icon` (name from a built-in icon set), `color`, and `fields:
Vec<FieldSpec>`. `FieldSpec`: `key`, `label`, `widget`, `required`,
`default`, `help`, plus widget-specific options. **Widget vocabulary (v1):**

| widget       | renders as                                        | value shape |
|--------------|---------------------------------------------------|-------------|
| `select`     | cycling chip / dropdown (`options: [..]`)         | string |
| `template`   | single-line expression editor (chips + literal)   | template string |
| `keyvalue`   | key + template-value rows (headers, body params)  | `[{key, value}]` |
| `number`     | numeric stepper (`min`, `max`)                    | number |
| `duration`   | seconds input with unit hint                      | number (sec) |
| `toggle`     | on/off                                            | bool |
| `text`       | plain string input (no expressions)               | string |
| `code`       | multi-line text (raw body, prompts)               | template string |

Template-bearing widgets get the expression picker automatically (inputs,
upstream outputs, variables, secrets, functions, iteration scope). An LLM
plugin would declare e.g. `model: select`, `prompt: code`, `temperature:
number` and implement `execute` — the builder UI, YAML round-trip,
validation plumbing, and run views all come for free.

**Core task envelope vs plugin config.** Generic concerns live outside the
plugin so every task type gets them uniformly:

```yaml
- id: scrape_minutes_index
  type: http.request          # plugin type_id
  retry: { type: exponential, max_attempts: 5, base_seconds: 30 }
  timeout_seconds: 60
  on_error: fail              # fail | continue (continue => item dropped in fan-out)
  config:                     # plugin-owned, shaped by manifest fields
    method: POST
    url: "{{ vars.server }}/api/scrape/index"
    headers: [{ key: Authorization, value: "Bearer {{ secrets.API_TOKEN }}" }]
    body: [{ key: since, value: "{{ inputs.since }}" }]
  outputs:
    - { name: documents, type: ARRAY, extract: result.body.documents }
```

`parallel` is a **core** task type (control flow, not a plugin): `items`
(template resolving to an array), `concurrency` (1–256), `tasks:` child list
where each child is any plugin task. Children run sequentially per item;
children reference `taskrun.value` (current item) and prior child outputs.
The parallel task's own outputs extract from `result.items` (array of each
item's final child result), e.g. `extract: result.items` — the implicit
"merge" from the mock.

**HTTP plugin (`http.request`), v1 scope:** method (GET/POST/PUT/PATCH/
DELETE), url (template), headers (keyvalue), body params (keyvalue →
JSON object) or raw body (code widget, mutually exclusive), success codes
(text, default `2xx`). Result JSON: `{ status, headers, body }` (body parsed
as JSON when possible, else string). Non-success status → TaskError (retried
per policy, then per `on_error`).

## 3. Expressions

Template strings: `{{ ref }}` segments interleaved with literal text; a
value that is exactly one `{{ ref }}` resolves to the referenced JSON value
(preserving type — arrays stay arrays); mixed templates stringify. Stored as
plain strings in definitions; the UI chip editor parses/serializes them.

References: `inputs.<id>` · `outputs.<task>.<name>` · `vars.<id>` ·
`secrets.<NAME>` · `taskrun.value[...]` (inside parallel children) ·
functions: `now()`; filters: `| dateAdd(<n>, 'DAYS'|'HOURS'|'MINUTES')`.
No `jq` in v1. Secrets resolve at execution time only and are redacted in
logs and stored task results (values replaced with `***` wherever the
resolved secret appears).

**Output extraction:** dotted path with array indices into the plugin result
(`result.body.ids`, `result.items`, `result.body.data[0].id`). Extraction
failure = task failure (clear error message).

## 4. Data model (SQLite)

- `flows` — id (slug PK), name, namespace, description, `definition` JSON,
  `paused`, current_rev, created/updated timestamps.
- `flow_revisions` — flow_id, rev (int), definition JSON, message, created_at.
  Every save appends; revision panel lists these; restore = save as new rev.
- `runs` — id (autoinc), flow_id, flow_rev, status (`queued|running|success|
  failed|canceled`), trigger (`schedule|manual|api`), inputs JSON,
  scheduled_for (nullable — the cron occurrence a catch-up run covers),
  started/finished, error.
- `task_runs` — run_id, task_id, status (+`skipped`), attempt, started/
  finished, `result` JSON (redacted), extracted `outputs` JSON, error.
- `task_run_items` — task_run_id, idx, item JSON (redacted), status, attempt,
  result JSON, error, started/finished. Powers the fan-out inspector
  (paginated + status-filtered queries, aggregate counts).
- `logs` — run_id, ts, level (`INFO|OK|WARN|ERR|DBG`), task, message.
- `schedule_state` — flow_id + trigger_id PK, enabled, next_fire_at,
  last_fired_at. Derived from definitions on save; toggled from Schedules UI.
- `secrets` — name PK, nonce+ciphertext, created/updated.

## 5. Execution & scheduling

**Executor.** In-process engine. A run executes tasks sequentially in
definition order (the builder only offers upstream references, so order is
the dependency chain). Per task: render templates from run context → plugin
`execute` with timeout → retry per policy (exponential backoff) → extract
outputs into context → log lifecycle events. Fan-out: `buffer_unordered
(concurrency)` over items; each item runs its child chain sequentially;
per-item retry; `on_error: continue` drops the item (WARN log), `fail` fails
the task after retries. Cancellation: per-run `CancellationToken`; cancel
endpoint flips run + in-flight tasks/items to `canceled`. Replay: new run
with the same inputs at current flow rev. Retry-failed-items (fan-out
inspector): re-executes failed items within a *new run*? No — v1 scopes it
to re-running failed items in-place while the run is still running is
complex; instead "Retry failed" creates a new run whose input is the failed
items' values (documented in UI copy). Runs that were `running` when the
process died are marked `failed` ("interrupted") on startup.

**Scheduler.** Tokio loop (tick ~1s, plus recompute on schedule changes):
fire any enabled schedule with `next_fire_at <= now`, insert queued run,
advance `next_fire_at` via croner in the trigger's timezone. **Catch-up:**
per-trigger `catchup: none | latest | all` (default `latest`). On startup —
and generically whenever `next_fire_at` is in the past — `none` skips to the
next future occurrence; `latest` queues one make-up run (for the most recent
missed occurrence, `scheduled_for` set accordingly); `all` queues one run per missed
occurrence (the most recent 100 when more were missed, with a WARN log). Concurrent runs of
the same flow are allowed; scheduler queues regardless of running state.

**Live updates.** SSE `GET /api/runs/:id/events`: snapshot event, then
status deltas (run/task/item-counters) + log lines. List screens poll
(3–5s). Fan-out inspector uses the same stream's item-counter events +
paginated item queries.

## 6. API (JSON, under /api)

- `GET /plugins` — manifests for the task palette + inspector rendering.
- `flows`: `GET /flows` (with 30d stats: success rate, avg duration, last
  run) · `POST /flows` · `GET/PUT/DELETE /flows/:id` (PUT takes optional
  revision message; creates rev) · `POST /flows/:id/pause` ·
  `GET /flows/:id/revisions` · `GET /flows/:id/revisions/:rev` ·
  `POST /flows/validate` (definition → errors;
  powers the valid/incomplete badge) · `GET /flows/:id/export` (YAML) ·
  `POST /flows/import` (YAML body).
- `runs`: `GET /runs?flow=&status=&page=` · `POST /flows/:id/run` (inputs)
  · `GET /runs/:id` (tasks + fan-out aggregates) · `POST /runs/:id/cancel`
  · `POST /runs/:id/replay` · `GET /runs/:id/logs?after=` ·
  `GET /runs/:id/events` (SSE) ·
  `GET /runs/:id/tasks/:task/items?status=&page=` ·
  `POST /runs/:id/tasks/:task/retry-failed`.
- `schedules`: `GET /schedules` · `POST /schedules/:flow/:trigger/toggle`.
- `secrets`: `GET /secrets` (names + timestamps only) · `PUT /secrets/:name`
  · `DELETE /secrets/:name`.
- `GET /dashboard` — metric cards (active flows, 24h runs, success rate,
  avg duration, next scheduled).

No auth in v1 (binds 127.0.0.1 by default; `--listen` to expose — README
warns).

## 7. UI

SvelteKit, Svelte 5 runes, pathname routing. Plain CSS with the mock's design
tokens as CSS variables; IBM Plex inline. Screens (mock-faithful visuals):

1. **Flows dashboard** — metric cards + flows table from real data; row →
   editor. New flow button.
2. **Flow builder/editor** (one screen for create *and* edit) — task canvas
   (trigger → tasks → add), inspector panel driven by plugin manifests
   (schema-rendered fields, expression chip editor with grouped picker),
   inputs/variables/trigger editors (cron + tz + catchup editable, trigger
   optional), core envelope controls (retry, timeout, on_error), outputs
   with extract paths, live YAML pane (generated, syntax-highlighted),
   valid/incomplete badge from `/flows/validate`, Save w/ revision message,
   revision history panel (restore), Run button, YAML export/import.
3. **Run detail** — header (status, elapsed, Cancel/Replay), metrics,
   execution graph (vertical chain + fan-out node w/ live progress), Logs
   (live tail via SSE) / Timeline (Gantt from task timings) tabs.
4. **Fan-out inspector** modal — aggregate stats, progress bar + ETA,
   in-flight items, canvas heatmap (statuses of all items), Failed/Retrying/
   Slow tabs from item queries, retry-failed action.
5. **Runs** — filter chips w/ counts, paginated table, → run detail.
6. **Schedules** — real cron descriptions ("Every day at 03:00"), next fire,
   last status, enable/disable toggles.
7. **Secrets** — names list, add/update (value write-only), delete.
8. **Trigger-run modal** — generated from the flow's declared inputs (typed
   widgets, required validation, defaults prefilled). Backfill mode deferred
   (post-v1) — catch-up covers the main need.

Dropped from mock: workers sidebar widget (replaced by simple "engine ok ·
N running" from real data), avatar, ⌘K search (deferred), backfill toggle.

## 8. Testing

- **expr**: parse/render/round-trip, type preservation, filters, redaction.
- **plugins/http**: wiremock — methods, headers, bodies, success-code
  handling, timeout, JSON vs text bodies.
- **engine**: wiremock end-to-end flows — sequential context passing,
  extraction, retries/backoff, fan-out concurrency + on_error semantics,
  cancellation, interrupted-run recovery.
- **scheduler**: next-fire math across timezones/DST, catch-up matrix
  (none/latest/all), toggle behavior.
- **api**: axum integration tests on temp DBs (CRUD, revisions, validate,
  SSE smoke, secrets redaction end-to-end).
- **ui**: vitest for template⇄chip parser and YAML view-model; the rest is
  covered by using the app (manual + `verify` flow).

## 9. Non-goals (v1)

Auth/multi-user · distributed workers · dynamic plugin loading (wasm/dylib)
· `jq` filters · DAG editing beyond linear + fan-out · backfill windows ·
namespace management (free-text label only) · ⌘K search · log retention
policies.
