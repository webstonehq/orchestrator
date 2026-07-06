# Bring Your Own Worker (BYOW): remote execution for Orchestrator

Date: 2026-07-06

## Goal

Let Orchestrator run as a **control plane** (UI, JSON API, scheduler, state)
while task execution happens on the user's **own machines** — a GPU box under
a desk, a private-network host, wherever the compute or data lives. The
central server keeps scheduling, observability, and durable state; workers
bring the execution.

The existing single-binary, executes-everything-in-process mode remains the
default and is unchanged for every existing flow.

## Decisions

- **Both granularities, one boundary.** Support offloading a whole run *and*
  (later) routing individual tasks to workers. Both use the same mechanism;
  phase 1 ships whole-run offload only.
- **Worker dials the server (pull-based).** Workers sit behind NAT (home/office
  GPU machines). The server needs no inbound reachability to a worker. Workers
  open a long-lived connection out to the server and pull work.
- **Secrets are local to the executing box.** A task's `{{ secrets.* }}`
  resolves against whatever machine runs it. The worker resolves its **own**
  secret store; plaintext secrets never travel to the server, and redaction
  happens on the worker before any data leaves it. The server's secret store
  serves only `local`-queue runs.
- **Named queues route work.** A flow declares `queue: gpu`. Workers subscribe
  to one or more queues and pull only matching work. Absent → `local`, served
  by the server's in-process worker. Multiple workers on one queue load-balance
  by first-come claiming.
- **The worker IS the engine.** Not a thin remote-plugin shim — the same binary
  runs the real `Engine` loop in a worker role. This is what makes
  "secrets local to the box" and full-flow offload fall out for free.

## The core inversion: one run-update stream

Today `engine::run::execute_run` does two things at each step: it writes state
to SQLite (`db.upsert_task_run`, `finish_task_run`, `insert_items`,
`update_item`, `append_log`, run status) **and** broadcasts a live `RunEvent`.

We collapse both into a single stream of **`RunUpdate`** messages emitted to a
`RunSink`. The server's entire job for any run — local or remote — becomes:
*apply a `RunUpdate` stream to SQLite and re-broadcast it to SSE subscribers.*

```
                        ┌───────────────────────────────┐
  in-process run  ─────▶│  execute_run emits RunUpdate   │
                        └───────────────┬───────────────┘
                                        │
                     LocalSink ─────────┤          RemoteSink (worker)
              apply() → SQLite          │            serialize → WS ──┐
              publish → broadcast        │                            │
                                        ▼                             ▼
                                  SSE subscribers          server applies same
                                  (UI live view)           apply() to SQLite +
                                                            broadcast (identical)
```

- **`LocalSink`** (server, `local` queue): `apply(update)` runs the exact DB
  write + broadcast that lives inline in `run.rs` today, just relocated.
- **`RemoteSink`** (worker): serializes each update with a per-run monotonic
  `seq` onto the WebSocket.
- **Server-side applier**: an arriving worker `RunUpdate` is fed through the
  *same* `apply()` `LocalSink` uses. In-process and remote runs converge on one
  persistence path. SQLite stays single-writer, owned by the server; workers
  never touch it.

## Roles: three modes, one binary

- `orchestrator serve` — control plane: UI, API, scheduler, SQLite, SSE
  fan-out, dispatch queue, worker WS endpoint, lease reaper. Runs an in-process
  worker permanently subscribed to `local`.
- `orchestrator worker --server https://… --queues gpu,default` — dials the
  server, authenticates, runs the real `Engine` against its own secret store,
  reports via `RemoteSink`.

## Routing and queues

- `queue` is a new optional flow field, validated like an id
  (`[a-z][a-z0-9_-]*`), round-tripping through YAML and the visual builder.
  Absent → `local`.
- `create_run` snapshots the flow's `queue` onto a new `queue` column on the
  run row (stable across re-queueing even if the flow's queue later changes).
- `queue = local` → server starts the run in-process immediately (as today).
- Any other queue → the run stays `queued`; a subscribed worker claims it on
  its next pull. No connected worker → the run sits `queued`, shown in the UI
  as waiting for that queue (the honest state).
- **Phase 2**: a task may carry its own `queue:` to override the flow's, so a
  flow can mix a server-side HTTP fetch with a worker-side GPU task.

## Wire protocol

- **Transport**: one long-lived **WebSocket** per worker
  (`GET /api/worker/connect`, upgraded), carrying claims, the `RunUpdate`
  stream, heartbeats, and cancel signals bidirectionally. Auth: bearer token in
  the upgrade request, checked against a server-config allowlist. TLS assumed
  for non-LAN.
- **Claim (lease-based)**:
  1. Worker → `Claim { queues, capacity }` (capacity = free slots).
  2. Server atomically leases up to `capacity` runs:
     `UPDATE runs SET status='leased', worker_id=?, lease_expires_at=now+30s
      WHERE status='queued' AND queue IN (…) LIMIT n`. The `status='queued'`
     guard makes concurrent claims race-safe — two workers never grab the same
     run; the loser's update hits zero rows and it retries.
  3. Server → `Assign { run_id, flow_definition, inputs, trigger }`. The flow
     definition and resolved inputs travel; secrets do not.
- **Report**: worker runs the engine loop, emitting `RunUpdate { run_id, seq, … }`
  for every transition.
- **Heartbeat**: `Heartbeat { run_ids }` every ~10s extends `lease_expires_at`.
- **Cancel**: UI cancel → server sends `Cancel { run_id }` → worker flips that
  run's `CancellationToken`, exactly as the in-process path does.

## Secrets, rendering, redaction

- **Phase 1 (whole-run)**: nothing splits. The worker's engine builds the full
  `{ inputs, vars, outputs, secrets }` context itself, with `secrets` from its
  **own `SecretStore`**, renders configs, executes, and redacts — all before
  any `RunUpdate` leaves the box. The server only ever persists/broadcasts
  already-redacted data. **No `expr` changes.**
  - Existing late-binding carries across the boundary for free: an input value
    referencing `secrets.*` is already stored as a raw template string on the
    run row, so it ships as a template and the worker renders it locally.
- **Phase 2 (mixed)**: the server renders `inputs/vars/outputs` into a
  worker-bound task config but leaves `{{ secrets.* }}` intact — one new,
  scoped `expr` capability: *render all paths except `secrets`* (built on the
  existing `expr::referenced_paths` + per-path rendering). Redaction stays
  split: each box redacts its own secrets from the values it produces.
- **Trust**: a worker token grants access to flow definitions + inputs on its
  queues and lets that machine see its own secrets in plaintext (it's the
  user's machine). Tokens are per-worker and revocable in server config; a
  compromised worker is contained to its queues.

## Failure handling and delivery semantics

- **Worker dies mid-run**: heartbeats stop, lease expires. A server-side
  **reaper** (periodic sweep, same shape as the existing `recover_interrupted`)
  finds `leased`/`running` runs past their lease and, by default, **fails**
  them (`"worker lost (lease expired)"`), marking unfinished tasks/items as an
  interrupted run does today. Opt-in **requeue** for flows marked idempotent is
  phase 3.
- **Server restart**: `recover_interrupted` already handles orphaned in-process
  runs; remote runs get the same treatment — any `leased`/`running` run with no
  worker reconnecting inside a grace window is failed.
- **At-least-once + idempotent apply**: every `RunUpdate` is keyed by
  `(run_id, seq)` and carries the **full target state** of a row (status,
  attempt, result, error), not a delta — so replays/reorders converge
  (idempotent upsert). The server tracks the highest applied `seq` per run and
  ignores stale ones. This mirrors today's last-write-wins `db.upsert_*`.
- **Claim race**: settled entirely by the `WHERE status='queued'` guard on the
  atomic `UPDATE`. No distributed lock or coordinator — SQLite's write
  serialization is the whole mechanism.
- **Backpressure**: `capacity` caps how many runs a worker leases; the server
  never pushes beyond what a worker asked for.

## Reconnection and live-view continuity

- **Worker reconnect**: capped exponential backoff. On reconnect the worker
  sends `Resume { active_run_ids, last_seq_per_run }`; the server re-extends
  leases and the worker flushes buffered `RunUpdate`s (server `seq` dedupe
  absorbs overlap). If a run was already reaped during a long outage, the
  server replies `Abandon { run_id }` and the worker cancels it locally.
- **UI is transport-agnostic**: the server always owns each run's broadcast
  channel, publishing to it from both the in-process executor and the applier
  for worker updates. `GET /api/runs/:id/events` behaves identically whether the
  run is `local` or remote; the DB remains authoritative and server-written, so
  the existing "live channel, else DB snapshot" fallback still holds. No UI
  changes.
- **Known tradeoff**: remote live logs are only as fresh as the connection; a
  brief disconnect delivers a burst of buffered lines on reconnect. Acceptable
  for v1; per-line acks would smooth it (phase 3).

## Code seams

Mapping to the current tree:

1. **`src/engine/run.rs`** — factor the two side-effects into a `RunUpdate`
   enum + `RunSink` trait. `execute_run` emits updates instead of calling
   `db`/`tx` directly. This is the linchpin change.
2. **`src/engine/`** — `LocalSink` (apply → SQLite + broadcast) and the
   server-side applier for remote updates share one `apply(update)`.
3. **New `src/worker/`** — client: dial, claim, run via `Engine`, report.
4. **New `src/api/worker.rs`** — WS endpoint, claim/lease/heartbeat handlers,
   server-side applier wiring.
5. **`src/db.rs`** — `queue` column, atomic lease `UPDATE`, per-run `seq`
   tracking, reaper query, migration.
6. **`src/main.rs`** — `worker` subcommand.
7. **`src/config.rs`** — worker-token allowlist (serve); server URL / token /
   queues (worker).
8. **`src/model/flow.rs` + validation** — the one `queue` field.

**Untouched**: the `TaskPlugin` trait and all plugins (`http.rs`), the `expr`
layer (phase 1), model/validation beyond `queue`, and the entire UI.

## Phasing

- **Phase 1 — Whole-run offload.** `queue` field; the `RunUpdate`/`RunSink`
  refactor with `LocalSink` (land it behavior-preserving before any network
  exists); WS endpoint + `worker` subcommand; claim/lease/heartbeat; reaper
  (expire→fail); worker resolves its own secrets. Outcome: mark a flow
  `queue: gpu`, run `orchestrator worker` on the GPU box, whole flow runs there
  with live UI streaming. No `expr` or plugin changes.
- **Phase 2 — Mixed flows.** Per-task `queue:` override + the
  render-except-secrets pass. The only phase touching `expr` and the task
  dispatch point.
- **Phase 3 — Operational polish.** Requeue-on-loss for idempotent flows;
  worker-status panel in the UI (connected workers, queues, load); per-line log
  acks.
