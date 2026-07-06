# BYOW Workers — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: superpowers:executing-plans, superpowers:test-driven-development.
> Companion to `2026-07-06-byow-workers-design.md`. Git commits are the user's
> responsibility (global rule forbids the assistant committing); "Commit" steps
> are omitted — each increment instead ends at a green `cargo test`.

**Goal:** Let execution happen on user-run workers (the same binary in a worker
role) that dial the control-plane server, claim queued runs by queue label, run
the real engine locally against their own secrets, and stream results back.

**Architecture:** Worker = same `Engine` in a worker role. `execute_run` emits a
single `RunUpdate` stream to a `RunSink`; the server's `LocalSink` applies it to
SQLite + broadcasts, and a `RemoteSink` ships it over a WebSocket for the server
to apply identically. Queues route work; leases + a reaper handle worker loss.

**Tech Stack:** Rust, axum 0.8, reqwest, rusqlite, serde.

> **Status (2026-07-06): built and green.** Increments A–D are implemented and
> tested (`cargo test` all green; `cargo clippy` clean).
>
> **One deliberate deviation from the design doc:** the transport is
> **authenticated HTTP polling**, not WebSocket. The worker still dials out and
> pulls (same architecture, same NAT traversal), but uses `POST
> /api/worker/{claim,updates,heartbeat}` over the existing `reqwest`/`axum`
> stack rather than adding a `tokio-tungstenite` WebSocket. This kept the
> dependency surface small and made the end-to-end path easy to test
> (`tests/worker_e2e.rs`). Cancellation is delivered on the `updates`/
> `heartbeat` responses instead of a server push. Moving to WebSocket later is
> a transport swap behind the same `RunUpdate` protocol — no engine changes.

---

## Increment A — Queue field + routing (behavior-preserving)

Foundation: a `queue` label on flows, snapshotted onto runs; the server's
in-process executor serves only `local`, other queues stay `queued`.

### Task A1: `queue` on `FlowDefinition`
- Modify `src/model/flow.rs`: add `pub queue: String` with
  `#[serde(default = "default_queue", skip_serializing_if = "is_local_queue")]`
  and helpers `default_queue() -> "local"`, `is_local_queue(&String) -> bool`.
  `skip_serializing_if` keeps the YAML fixture test byte-identical for
  default-queue flows.
- Test (`tests/` or inline): a definition JSON without `queue` deserializes to
  `queue == "local"`; one with `queue: "gpu"` round-trips; serializing a
  `local` flow omits the key.

### Task A2: validate the queue label
- Modify `src/model/validate.rs`: in `validate`, if `def.queue` is not
  `is_valid_id`, push `{path: "queue", message: ...}`. `local` is valid.
- Test: `validate` rejects `queue: "GPU!"`, accepts `queue: "gpu_1"`.

### Task A3: `queue` column on `runs`
- Modify `src/db.rs`: add `MIGRATION_002` =
  `ALTER TABLE runs ADD COLUMN queue TEXT NOT NULL DEFAULT 'local';`
  plus `CREATE INDEX idx_runs_queue ON runs(queue, status);`. Register in
  `MIGRATIONS`. Add `pub queue: String` to `RunRow`; update `map_run` (append
  column to every `runs` SELECT). Change `insert_run` to take `queue: &str`.
- Test: open a fresh Db, `insert_run(..., "gpu")`, `get_run` returns
  `queue == "gpu"`; default migration path yields `local` for legacy rows.

### Task A4: engine + scheduler pass the queue; routing in `start`
- Modify `src/engine/mod.rs`: `create_run` reads `def.queue`, passes to
  `insert_run`. `start` loads the run; if `run.queue != "local"`, log
  "awaiting <queue> worker" and return `Ok(())` WITHOUT spawning (leave queued).
- Modify `src/scheduler.rs`: `launch_run` parses the flow def (already has
  `flow`) to get the queue and passes it to `insert_run`.
- Test: `create_and_start` on a `gpu` flow leaves the run `queued` and spawns
  nothing (`active_run_count() == 0`); a `local` flow still runs to `success`.

### Task A5: expose `queue` in the runs API
- Modify `src/api/runs.rs`: add `queue` to `RunSummary`, populate in
  `run_summary`.
- Test: existing run-detail integration test still green; add assertion that
  the payload includes `queue`.

Green gate: `cargo test`.

---

## Increment B — Lease/claim/reaper DB layer (server side)

### Task B1: lease columns
- `MIGRATION_003`: add `worker_id TEXT`, `lease_expires_at TEXT`,
  `last_seq INTEGER NOT NULL DEFAULT 0` to `runs`. Add `leased` as an accepted
  status value (string, no enum change needed).

### Task B2: atomic claim
- `Db::claim_runs(worker_id, queues: &[&str], capacity, lease_secs) ->
  Vec<RunRow>`: single `UPDATE ... WHERE status='queued' AND queue IN (...)
  ORDER BY id LIMIT capacity RETURNING ...`. Race-safe via the status guard.
- Test: two concurrent claims over the same queue never return the same run id;
  claimed rows become `leased` with a future `lease_expires_at`.

### Task B3: renew + reap
- `Db::renew_leases(worker_id, run_ids, lease_secs)`.
- `Db::reap_expired_leases(now) -> Vec<i64>`: `leased`/`running` runs past lease
  → `failed` (+ mark task_runs/items like `mark_interrupted`), returns ids.
- Test: a lease in the past is reaped to `failed`; a fresh lease is untouched.

### Task B4: seq tracking
- `Db::bump_seq(run_id, seq) -> bool` (accepts only strictly-increasing seq).
- Test: out-of-order/duplicate seq rejected.

Green gate: `cargo test`.

---

## Increment C — `RunUpdate` / `RunSink` refactor (internal, behavior-preserving)

### Task C1: define the types
- New `src/engine/update.rs`: `enum RunUpdate { RunStatus, TaskUpsert,
  TaskFinish, ItemsInsert, ItemUpdate, Items, Log }` (full-state per row),
  `#[derive(Serialize, Deserialize)]`. Trait `RunSink { fn send(&self, u:
  RunUpdate); }`.
- `LocalSink { db, tx }` with `fn apply(db, tx, u)` performing the exact DB
  write + broadcast currently inline in `run.rs`.

### Task C2: route `execute_run` through the sink
- Refactor `src/engine/run.rs` so every `engine.db.*` + `tx.send` pair becomes
  one `sink.send(RunUpdate::…)`. Keep semantics identical.
- Green gate: the entire existing engine test suite passes unchanged.

---

## Increment D — WebSocket transport + worker subcommand

### Task D1: protocol types
- New `src/worker/proto.rs`: `ClientMsg { Claim, Heartbeat, Update, Resume }`,
  `ServerMsg { Assign, Cancel, Abandon, Ack }`. serde-tagged JSON.

### Task D2: server endpoint
- New `src/api/worker.rs`: `GET /api/worker/connect` (axum WS upgrade), bearer
  auth against config allowlist; claim loop, apply incoming `Update`s via
  `LocalSink::apply`, push `Cancel` from a per-worker channel; reaper task.

### Task D3: worker client + CLI
- New `src/worker/mod.rs`: dial, authenticate, claim, run via `Engine` with a
  `RemoteSink`, heartbeat, reconnect. `src/main.rs`: `worker` subcommand.
- `src/config.rs`: worker-token allowlist (serve) + server URL/token/queues
  (worker).

Green gate: `cargo test`, plus a manual end-to-end (`serve` + `worker`) smoke.
