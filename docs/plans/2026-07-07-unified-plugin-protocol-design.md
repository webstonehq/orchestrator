# Unified plugin protocol — design

**Date:** 2026-07-07
**Status:** Design agreed, ready for implementation planning
**Supersedes:** the execution model in
`docs/plans/2026-07-06-external-binary-plugins-design.md` (discovery, bundle
format, and capability advertisement from that doc still hold).

## Problem

Today there are **two** ways a task type executes:

1. **Built-in** — a Rust `TaskPlugin` trait object run in-process (`http.request`
   via `HttpPlugin`, sharing one `reqwest` client).
2. **External** — a bundle binary spawned per task over a stdio protocol.

Two execution paths is complexity we want to shed, and it makes built-in plugins
a privileged class rather than first-class citizens of the same system we ask
third parties to target. The goal: **one execution path — the stdio protocol —
for every task type**, with `http` demoted to "just another plugin."

The catch surfaced during design: `http` is the hot path. A representative run
fans out **2–3k items across ~15 hosts**. In-process it reuses a warm pool of a
few dozen connections; naive spawn-per-task would pay **2–3k cold TLS
handshakes** and run up to 256 concurrent processes — a large regression. So a
single "spawn a process per task" protocol is not enough on its own.

## Decisions

1. **One wire protocol, two lifecycles.** Every task type is an external plugin
   over the stdio protocol. The `TaskPlugin` trait and in-process execution are
   removed. A plugin declares a **lifecycle** in its manifest:
   - `oneshot` (default) — spawned fresh per task; trivial to author; naturally
     isolated; cancellation = kill the process. (Today's external model.)
   - `persistent` (opt-in) — one long-lived process holds warm state (e.g. a
     pooled `reqwest` client) and services many concurrent per-task requests,
     multiplexed by request id.
2. **`http` is a `persistent` plugin**, restoring connection pooling and
   eliminating per-task spawn on the hot path.
3. **`http` ships as a standalone plugin bundle** — its own binary, discovered
   from the plugins directory exactly like a third-party plugin. There is a
   **single plugin mechanism** (external bundles): no compiled-in plugins, no
   self-exec, no special-casing anywhere in the engine. The cost is that
   single-binary deploy is given up — a release ships the `orchestrator` binary
   plus a `plugins/` directory (see [Distribution](#distribution--onboarding)).
4. **Cargo workspace** holds the app and the provided plugins together, plus a
   shared `plugin-sdk` for authoring Rust plugins.
5. **UI/GitHub-release marketplace is deferred** (see end). Because `http` is
   already just a bundle, the marketplace is a pure add-on — a way to *fetch*
   bundles — with nothing to re-architect.

## Workspace layout

The app stays the workspace root package (minimal churn to existing `src/`);
the SDK and the provided plugin are members under `plugins/`.

```
Cargo.toml            # root: [package] orchestrator + [workspace] members
plugins/
  sdk/                # plugin-sdk: protocol types + a Rust authoring run-loop
  http/               # plugin-http: a standalone binary crate (the http plugin)
src/                  # the orchestrator app (unchanged location)
```

- **`plugin-sdk`** — the wire protocol types (`Request`, `Event`, `Lifecycle`,
  log levels), the `Plugin` authoring trait, and a `serve(plugin)` helper that
  runs the oneshot or persistent loop. Depended on by `orchestrator` (it needs
  the wire types to talk to plugins) and by every Rust plugin author.
- **`plugin-http`** — a **binary** crate whose `main()` is essentially
  `plugin_sdk::serve(HttpPlugin)`. `HttpPlugin` implements `plugin_sdk::Plugin`
  (async `execute`, a shared `reqwest` client); the logic is the existing
  `src/plugins/http.rs`, moved out. Depends on `plugin-sdk` + `reqwest`. Built
  and shipped as a bundle; **`orchestrator` does not depend on it.**
- **`orchestrator`** — depends on `plugin-sdk` for the wire types used by the
  plugin host. Gains no plugin-specific code beyond the host. (It keeps
  `reqwest` for its own worker→server transport, unrelated to the http plugin.)

## The `Plugin` trait (authoring) vs. the protocol (execution)

Two distinct things that today are conflated in `TaskPlugin`:

- **The protocol** is the execution seam the engine talks to — JSON over
  stdin/stdout. It's the *only* thing the engine knows. Language-agnostic.
- **`plugin_sdk::Plugin`** is an *authoring convenience* for writing plugins in
  Rust: `manifest()`, `validate(config)`, async `execute(ctx, config)`. It is
  **not** the engine's execution seam. `plugin_sdk::serve` turns a `Plugin` into
  a protocol-speaking process. Non-Rust authors skip it and speak the protocol
  directly.

So `http` is written against `plugin_sdk::Plugin` (familiar, like the old
trait), and the engine reaches it only through the protocol.

## Distribution & onboarding

`http` is a bundle like any other, so it has to reach disk. The default
`--plugins-dir` is `plugins/` beside the binary (from the prior design), so:

- **Release artifact** — a tarball containing the `orchestrator` binary and a
  `plugins/http/` bundle (its `plugin.json` + the `orchestrator-plugin-http`
  binary, built for the same target). Dropped in place, `http` is discovered at
  startup with no extra step.
- **Dev / build** — the build (mise task / script) compiles `plugin-http` and
  stages the bundle into the dev plugins-dir, so `mise run dev` still has `http`.
  A bare `cargo build` of just the app won't have it — that's the trade for one
  uniform mechanism.
- **Workers** — a worker needs the `http` bundle in *its* plugins-dir too, same
  as any plugin; the release tarball carries it. The existing capability
  advertisement + coverage/skew checks already flag a worker that lacks it.

Giving up single-binary deploy is the accepted cost of a single plugin
mechanism. A future marketplace (deferred) would automate fetching bundles onto
the server and workers.

## Unified registry

`PluginRegistry` stops holding `Arc<dyn TaskPlugin>` and holds `PluginEntry`
descriptors keyed by `type_id`, from **one** discovery source —
`load_bundles(plugins_dir)` (unchanged from the prior design, now also reading
the `lifecycle` field):

```rust
PluginEntry {
    manifest,                        // from plugin.json
    lifecycle,                       // Oneshot (default) | Persistent
    command: Command { program: bundle_dir.join(entrypoint), args: vec![] },
    version, supports_validate,
}
```

`http` and a third-party plugin produce the *same* `PluginEntry` shape; the
engine cannot tell them apart. Everything downstream that reads the registry —
`/api/plugins`, `flow_json_schema`, save-time validation, capability
advertisement (`type_id`/`version`), the coverage/skew checks — is unchanged; it
only ever needed the manifest + metadata, which `PluginEntry` carries.

## The plugin host

A `PluginHost` owned by the engine replaces direct `plugin.execute(...)` calls.
Its surface, per task:

```rust
async fn execute(&self, entry: &PluginEntry, req: Request, ctx: &TaskContext)
    -> Result<Value, TaskError>;
```

Internally it branches on `entry.lifecycle`:

- **Oneshot** — the existing `ExternalPlugin::execute`: spawn `entry.command`,
  write the request, read newline-delimited events, map `log` → `ctx.log`,
  `result`/`error` → outcome. Cancellation = `SIGTERM`→`SIGKILL`.
- **Persistent** — dispatch to a managed long-lived process (below).

The engine run loop (`engine/run.rs`) changes from
`registry.get(type_id) → plugin.execute` to
`registry.get(type_id) → host.execute(entry, req, ctx)`. Everything else
(retries, timeouts, redaction, output extraction) is unchanged.


## Wire protocol

Shared framing: newline-delimited JSON. `stdout` is the protocol channel;
`stderr` is free-form diagnostics.

**Oneshot** (unchanged from the prior design): one request in, a stream of events
out, terminal `result`/`error`, process exits. No request id needed.

**Persistent**: requests and events are **id-tagged** so many can be in flight.

Engine → plugin (stdin), many over the process lifetime:

```json
{ "id": 17, "mode": "execute", "run_id": 42, "task_id": "fetch", "config": { … } }
{ "id": 17, "mode": "cancel" }
```

Plugin → engine (stdout), each event carrying its request `id`:

```json
{ "id": 17, "type": "log", "level": "info", "message": "GET https://…" }
{ "id": 17, "type": "result", "value": { "status": 200, … } }
{ "id": 17, "type": "error", "message": "…", "retryable": true }
```

- The plugin runs each `execute` concurrently on its own async runtime against
  shared warm state (the pooled client), streaming id-tagged events as it goes.
- **Cancellation** is a message, not a signal: `{id, mode:"cancel"}`; the plugin
  stops that task and emits a terminal event for that `id`.
- **Startup handshake**: on spawn the engine sends `{mode:"hello",
  protocol_version}` and waits for `{type:"ready"}` before dispatching, so a
  broken binary fails fast.

The `oneshot`↔`persistent` difference is confined to the SDK's `serve` loop and
the host; the config/log/result/error payloads are identical.

## Persistent process manager

One manager per persistent plugin, per executor process (the server, and each
worker — a worker runs `http` from the `http` bundle in its own plugins-dir).

- **Start** (lazily on first use): spawn `entry.command` with piped stdio; run
  the hello/ready handshake; start a **reader** task that demuxes stdout events
  to per-request channels by `id`, and serialize stdin writes.
- **Dispatch**: allocate an `id`, register a channel, write the request, forward
  events to the caller until terminal, deregister. Concurrent dispatches
  multiplex over the one process; the engine's own concurrency cap (≤256) bounds
  in-flight requests.
- **Cancellation**: send `{id, mode:"cancel"}`, await the terminal event.
- **Hard timeout**: a persistent process can't be killed for one task without
  killing siblings. So on timeout the host first sends `cancel`; if the plugin
  doesn't yield within a grace, the host **tears down the whole process**
  (`SIGKILL`) and fails all in-flight requests as **retryable** — the engine
  retries them on a freshly-started process. This preserves the engine's timeout
  guarantee at the cost of collateral retries, and is documented as the contract
  (a well-behaved persistent plugin honors `cancel` and never triggers it).
- **Crash / exit**: all in-flight ids get a terminal **retryable** error; the
  process restarts on the next demand. A restart storm is bounded (cap restarts
  in a window; beyond it, fail fast with a clear error).
- **Shutdown**: close stdin (EOF) → the plugin drains and exits; `SIGKILL`
  fallback after a grace.

## Manifest change

`plugin.json` (and the bundled compile-time manifest) gain one field:

```json
{ "lifecycle": "persistent", "…": "…" }
```

`oneshot` is the default when omitted, so every existing bundle keeps working
untouched. All other fields (`schema_version`, `name`, `version`, `entrypoint`,
`supports_validate`, `manifest`) are unchanged.

## `http` as `plugin-http`

- The bundle's `plugin.json` carries `lifecycle: "persistent"`,
  `supports_validate: true`, and today's `PluginManifest` verbatim
  (method/url/headers/body/raw_body/success_codes).
- `HttpPlugin` implements `plugin_sdk::Plugin`; `execute` is the existing http
  logic (success-code parsing, retryable classification, cancellation via the
  SDK ctx). The crate's `main()` is `plugin_sdk::serve(HttpPlugin)`. The
  `reqwest` client is built **once per process** and shared across all
  concurrent requests → pooling and TLS-session reuse restored, so the 2–3k
  fan-out reuses a few dozen connections instead of re-handshaking.
- The existing http unit tests (wiremock) move to `plugin-http`, testing the
  `Plugin` impl directly; a protocol-level test drives it through `serve`.

## What gets deleted

- `src/plugins/mod.rs`: the `TaskPlugin` trait, `TaskContext` as a trait input
  seam (kept as the host's logging/cancel context), the `Arc<dyn TaskPlugin>`
  registry.
- `src/plugins/http.rs`: moves to `plugins/http/`.
- The in-process execution branch in `engine/run.rs`.

Nothing is added to replace them beyond the plugin host — no self-exec, no
compile-time plugin table. Net: the engine has one execution path (the host),
one discovery source (bundles), and `http` is no longer special.

## Error handling summary

- Oneshot: as today (crash/protocol-violation/timeout → fatal or kill).
- Persistent: per-request errors are isolated to their `id`; a corrupt/unframable
  stream, process crash, or unresponsive-to-cancel timeout tears down the
  process and fails in-flight requests **retryable** (engine retries on a fresh
  process). Startup/hello failure → the plugin's tasks fail "plugin unavailable".

## Testing

- **plugin-sdk**: the `serve` loop for both lifecycles; framing; id correlation;
  concurrent dispatch; `cancel`; a fixture `Plugin`.
- **plugin-http**: the http logic (wiremock) against the `Plugin` impl; a
  protocol round-trip test.
- **persistent host**: N concurrent requests to one fixture persistent process →
  assert correlation and true concurrency; cancel one id mid-flight; process
  crash → in-flight fail retryable + auto-restart; unresponsive-to-cancel →
  teardown + retryable.
- **end-to-end**: a flow using `http` (persistent, standalone bundle) runs to
  success; a
  fan-out over many items to a wiremock server is served by a **single** plugin
  process with **reused connections** (assert one process; assert handshake/conn
  count stays bounded).
- Migrate the existing external-plugin (oneshot) tests unchanged.

## Sequencing

1. Introduce the workspace + `plugin-sdk` (protocol types, `Plugin` trait,
   `serve`). Move the http logic into `plugin-http` as a `Plugin` impl behind a
   thin in-process adapter so the app keeps working unchanged.
2. Unify the registry onto `PluginEntry` and route execution through the oneshot
   host (external bundles already use it).
3. Add the persistent host + manager and the `lifecycle` field.
4. Build `plugin-http` as a standalone bundle (`lifecycle: persistent`), stage
   it into the plugins-dir, point the engine at it, and remove the `TaskPlugin`
   trait, the in-process adapter, and the dead code.

Each step keeps the suite green; `http` behavior is preserved throughout.

## Deferred (unchanged intent)

- **UI marketplace / GitHub-release install** — arch-matched download +
  checksum/signature verify, plugins-dir write, and runtime plugin (re)load.
  The protocol is identical for provided and third-party plugins, so this is a
  later add-on layer, not a re-architecture. Provided plugins ship compiled-in
  in the meantime (works out of the box).
- **Long-lived process pools / per-process in-flight caps** — start with one
  process per persistent plugin bounded by the engine's concurrency; revisit if
  a single process becomes a bottleneck.
- **Windows** — signal-based teardown is unix-only (as in the prior design).

## Open questions

- **Restart-storm policy** — exact cap/window for auto-restart before failing
  fast.
- **Eager vs lazy** persistent start — lazy on first use (simpler, no cost when
  unused) vs eager at startup (surfaces a broken plugin immediately). Leaning
  lazy.
