# External binary plugins — design

**Date:** 2026-07-06
**Status:** Design agreed, ready for implementation planning

## Problem

Today a "plugin" (task type) is a compiled-in Rust `TaskPlugin` registered in
`PluginRegistry::builtin()`. Adding a task type means recompiling the
orchestrator binary. We want third parties to supply plugins as **binaries** that
the orchestrator **discovers on startup** — including the UI a new task type needs
in the flow editor — without forking or rebuilding the orchestrator.

## Decisions (the resolved forks)

These were settled during design and frame everything below:

1. **Out-of-process, local subprocess model** (not remote workers) — plugins are
   binaries dropped into a folder beside the orchestrator, not network services.
2. **Declarative UI only** — a plugin's UI is its `PluginManifest`
   (`FieldSpec`s over the existing widget set: `Select | Template | Keyvalue |
   Number | Duration | Toggle | Text | Code`). Plugins ship **no frontend code**.
   If a plugin needs a new *kind* of input, the shared widget enum grows in-tree.
3. **Spawn-per-invocation** — each task execution `exec`s the plugin fresh; no
   long-lived processes, no request multiplexing. Cancellation = kill the process.
4. **Bundle directory format (D2)** — a plugin is a directory with a `plugin.json`
   (metadata + manifest) plus an executable. The manifest is readable **without
   executing** anything.

## Core architectural insight: the trait is the seam

`TaskPlugin` is already a trait object. `PluginRegistry` holds
`Arc<dyn TaskPlugin>` in a `BTreeMap`, and every consumer — the engine's
`execute`, `GET /api/plugins`, `GET /api/flow.schema.json`, save-time
validation — talks to the trait, never to `HttpPlugin` concretely.

So external plugins need **no new subsystem**. We add one more implementation of
the existing trait:

```rust
struct ExternalPlugin {
    manifest: PluginManifest,   // parsed from plugin.json at startup
    entrypoint: PathBuf,        // the bundle's executable
    bundle_dir: PathBuf,        // cwd for execution; lets plugins find sibling assets
    version: String,            // from plugin.json; used for capability reporting
}

impl TaskPlugin for ExternalPlugin {
    fn manifest(&self) -> PluginManifest { self.manifest.clone() }
    fn validate(&self, cfg: &Value) -> Vec<String> { /* schema-derived checks */ }
    async fn execute(&self, ctx: &TaskContext, cfg: Value)
        -> Result<Value, TaskError> { /* spawn entrypoint, speak wire protocol */ }
}
```

Registered as just another `Arc<dyn TaskPlugin>` next to `HttpPlugin`, the entire
rest of the codebase is **untouched** — UI rendering, schema/autocomplete,
validation, retries, cancellation, and parallel fan-out all keep working because
they only ever knew the trait.

The whole feature reduces to: **one new trait impl + one directory scan + a wire
protocol.**

## On-disk bundle format (D2)

```
<plugins-dir>/
  slack-notify/
    plugin.json        # metadata + full PluginManifest — the ONLY thing read at startup
    slack-notify       # the executable (entrypoint)
    ...                # optional sibling assets (cwd is the bundle dir)
```

`plugin.json`:

```json
{
  "schema_version": 1,
  "name": "slack-notify",
  "version": "0.2.0",
  "entrypoint": "slack-notify",
  "manifest": {
    "type_id": "slack.notify",
    "label": "Slack Notify",
    "description": "Post a message to a channel",
    "icon": "message-square",
    "color": "#4A154B",
    "fields": [
      { "key": "channel", "widget": "Text",     "required": true, "help": "…" },
      { "key": "text",    "widget": "Template", "required": true, "template": true }
    ]
  }
}
```

The `manifest` block is exactly the existing `PluginManifest` / `FieldSpec` /
`Widget` shape, now `#[derive(Deserialize)]` in addition to today's `Serialize`.

**Location:** default `plugins/` beside the binary, overridable via `--plugins-dir`
flag / env var. Discovered by the `serve` process (for UI + local-queue execution)
and by each `worker` process (for its own execution).

## Startup discovery

Scan `<plugins-dir>/*/plugin.json`, deserialize each, and register an
`ExternalPlugin`. This is D2's payoff: **zero code execution at startup** — we read
what a plugin claims before ever running it.

**Graceful degradation — discovery must never crash the process:**

- Malformed/unreadable `plugin.json` → log a warning, **skip**, keep booting.
- `schema_version` newer than supported → skip with a clear warning.
- Duplicate `type_id` (collides with a builtin or another bundle) → skip the loser,
  warn. *(Today `register()` `panic!`s on duplicates — external registration must
  use a non-panicking path, e.g. `try_register() -> Result`.)*
- Missing/again-non-executable entrypoint file → skip; a plugin that cannot execute
  must not appear in the UI.

## Wire protocol (execution)

Spawn-per-invocation. The engine already wraps `execute` in
`tokio::time::timeout` (default `DEFAULT_TIMEOUT_SECS = 60`, or the task's
`timeout_seconds`) and passes a child `CancellationToken` — external plugins inherit
that machinery unchanged.

**Process setup**
- `cwd` = the bundle directory (so plugins can load sibling assets).
- Request delivered on **stdin** as a single JSON object, then stdin is closed.
  (stdin, not argv/env, so resolved secrets never appear in `ps`.)
- `stdout` = **protocol only** (newline-delimited JSON events).
- `stderr` = free-form diagnostics, captured and surfaced on failure / streamed to
  `dbg`.

**Request** (server → plugin stdin):

```json
{
  "protocol_version": 1,
  "run_id": "…",
  "task_id": "…",
  "config": { "…": "fully rendered config — all {{ }} already resolved" }
}
```

**Response** (plugin → stdout, newline-delimited JSON):

```json
{"type":"log","level":"info","message":"Resolving channel…"}
{"type":"log","level":"ok","message":"Posted (ts=1720…)"}
{"type":"result","value":{"ts":"1720…","channel":"C123"}}
```

or, terminally, an error:

```json
{"type":"error","message":"channel not found","retryable":false}
```

- `level` ∈ `{info, ok, warn, err, dbg}` → mapped 1:1 onto
  `ctx.info/ok/warn/err/dbg`, streaming live to the run view.
- The stream ends at the first `result` or `error` event; the process should then
  exit 0.
- Declared `outputs` are extracted from the `result.value` by the engine exactly as
  for builtin plugins; the engine still **redacts** the result (existing behavior).

**Failure / exit semantics**
- Process exits before any terminal event → `TaskError::fatal`, stderr included.
- Nonzero exit with no terminal event → `TaskError::fatal`, stderr included.
- Unparseable line on stdout → protocol violation → `TaskError::fatal`.
- `error` with `retryable: true` → feeds the engine's existing exponential-backoff
  retry loop.

**Cancellation & timeout**
- On cancel-token fire or timeout: `SIGTERM` the process (group), grace period
  (~3s), then `SIGKILL`. Because each invocation is its own process, this is clean
  and total.
- The engine already derives *canceled vs. failed* from its own token, not from the
  plugin's output — no change needed.

## Validation

Save-time validation stays **schema-derived** (no execution): required fields,
`Select` options, `Number` min/max, template-allowed flags — all already implied by
the manifest. `validate_plugin_task` (`src/model/validate.rs:313`) keeps delegating
to `plugin.validate(&cfg)`; `ExternalPlugin::validate` implements the schema checks
against its manifest.

Custom, plugin-authored validation (execing the binary at save time with the
*unrendered* config) is **deferred** — it reintroduces startup/save-time code
execution for marginal benefit.

## Secrets / trust boundary

`execute` receives **fully-rendered** config, so `{{ secrets.X }}` arrives as
plaintext on the plugin's stdin. This is the **same trust level as a BYOW worker**,
which already runs with secret access. Delivering the request on stdin (not argv or
env) keeps secrets out of process listings. The engine continues to redact returned
results. Execing binaries the operator deliberately placed in `plugins-dir` is the
accepted trust boundary for this personal / open-source tool — no sandboxing in v1.

## Plugins across server + workers

The asymmetry: the **manifest** lives on the server (renders UI, builds schema,
validates saves), but **execution** runs wherever the flow's `queue` routes it —
`local` in the server process, any other queue on a subscribed worker. So a task
authored against the server's manifest may be dispatched to a worker with no
matching bundle. Three layers, must-have → nice-to-have:

**1. Honest execution-time failure (required backstop).**
A worker builds its registry from its own `--plugins-dir`. Claiming a run whose task
`type_id` it lacks yields
`TaskError::fatal("no plugin 'slack.notify' installed on this worker")`, surfaced in
the run view. Never a silent hang.

**2. Author-time capability advertisement (recommended).**
Workers already dial in via claim/heartbeat. Piggyback a lightweight capability list
— `[{ type_id, version }]` for each loaded plugin — on that channel. The server
aggregates it **per queue** in memory, tied to the lease (a disconnected worker
drops its capabilities). Save-time validation then cross-checks: if a task routes to
queue `email` and no connected worker there advertises its `type_id`, emit a
**warning** in the editor ("no worker on queue `email` currently provides
`slack.notify`"). Warning, not error — a worker may connect later. Turns a 2am run
failure into author-time feedback, reusing the existing worker channel (no new
transport). Optionally warn on `version` skew between server manifest and worker
binary.

**3. Auto-distribution (explicitly deferred).**
The server could ship bundles to workers on demand, but server (e.g. macOS) and
worker (e.g. Linux) typically need **different-arch binaries**, plus
integrity/signing — a real subsystem. Installing bundles per-executor is the honest
v1 boundary.

## Codebase changes (file-by-file)

- **`src/plugins/mod.rs`** — add `#[derive(Deserialize)]` to `PluginManifest` /
  `FieldSpec` / `Widget`; add a non-panicking `try_register`; add a discovery entry
  point (e.g. `PluginRegistry::load_external(dir)`), keeping `builtin()` as-is.
- **`src/plugins/external.rs` (new)** — `ExternalPlugin`, the wire-protocol
  request/event types, the spawn + stream + kill logic, schema-derived `validate`.
- **`src/main.rs` (~209)** — add `--plugins-dir` to `serve`; build the registry as
  `builtin()` + `load_external(plugins_dir)`.
- **`src/worker/mod.rs` (~71)** — same registry construction; collect and report the
  worker's plugin capability list.
- **`src/api/worker.rs` + `Engine::claim_remote` / `heartbeat_remote`
  (`engine/mod.rs`)** — carry `[{ type_id, version }]`; store per-queue capabilities
  keyed to the lease.
- **`src/model/validate.rs` (~313)** — cross-check task `type_id` against the target
  queue's advertised capabilities → warning.
- **`Cargo.toml`** — process-group kill on unix may need `libc`/`nix` (or set the
  child's process group and signal it); confirm during implementation.

Unchanged (the payoff): the engine (`engine/run.rs`), `GET /api/plugins`,
`GET /api/flow.schema.json` / `model/schema.rs`, and the entire `ui/` app.

## Testing

- **Reference bundle fixture** under `tests/fixtures/` — a tiny plugin (script or
  small binary) that echoes config and emits a couple of log events; drives the full
  spawn → protocol → live-log → result path as an integration test.
- **Discovery** unit tests: valid / malformed / duplicate `type_id` /
  missing-entrypoint / newer `schema_version` all behave per the degradation rules.
- **Wire parser** unit tests: log events, terminal `result`, terminal `error`,
  unparseable line, premature EOF, nonzero exit.
- **Cancellation / timeout** integration test: a plugin that sleeps is `SIGKILL`ed;
  the run is marked canceled (not failed) via the engine's token.
- **Capability advertisement**: a worker reports its plugins; save-time validation
  warns when a task routes to a queue lacking the `type_id`.

## Implemented beyond the first prototype

The following were deferred in the original prototype and have since landed
(unix-only; Windows still excluded):

- **Graceful cancellation** — cancelling a plugin sends `SIGTERM`, then `SIGKILL`
  after a grace period (`TERM_GRACE`, 3s), instead of an immediate kill.
  `src/plugins/external.rs::ExternalPlugin::terminate`.
- **Plugin-authored save-time validate** — opt-in via `"supports_validate": true`
  in `plugin.json`. `validate()` sends the *authored* config in a
  `mode: "validate"` request and merges the plugin's `{"type":"validation",
  "errors":[...]}` reply with the schema-derived checks. Bounded by a watchdog
  (`VALIDATE_TIMEOUT`, 3s); infra failure falls back to schema-only rather than
  blocking the save. Plugins that don't opt in are never executed at save time.
- **Worker capability advertisement (design item 1, all three layers)**:
  - Layer 1 — a worker lacking a task type fails the run cleanly ("unknown task
    type"), never hangs (`engine/run.rs`).
  - Layer 2 — workers load their own `--plugins-dir` and advertise
    `[{type_id, version}]` on every `claim`; the server stores it per worker
    (`WorkerInfo.capabilities`), exposes it on `/api/workers`, and answers
    `Engine::queue_capability_list(queue)`. `model::coverage_report` splits the
    findings: **missing coverage** (no worker on the queue provides a task type)
    is an advisory *warning*; **version skew** (a worker runs a different version
    than the server's own copy) is a *blocking error*. Both surface in the
    `POST /api/flows/validate` response (`errors` / `warnings`); skew also blocks
    create/update saves (422).
  - Layer 3 (auto-distribution) remains deferred.

## Still out of scope (deferred)

- Long-lived / warm plugin processes (B2).
- Custom frontend widgets beyond the shared enum.
- Auto-distribution of bundles to workers; plugin signing/sandboxing.
- **Windows**: `.exe` entrypoint resolution and signal semantics — `terminate`
  and the validate watchdog use `libc` signals under `#[cfg(unix)]`, with a
  bare-kill fallback elsewhere.

## Resolved

- **Version skew** between the server's manifest and a worker's binary — a
  worker on the target queue running a different `version` than the server's own
  copy is a **blocking** validation error (align the versions). Missing coverage
  (no worker yet) stays advisory.
