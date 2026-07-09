# Unified execution: one claim/lease mechanism for local and remote runs

Date: 2026-07-09

## Motivation

Local (in-process) and remote (BYOW) runs behave differently today, and that
asymmetry forces every lifecycle feature to be built twice. The trigger was
run-level retry-on-worker-loss: with the current split it would need one
implementation for the reaper (remote) and another for startup recovery
(local).

The execution *core* is already shared — `run::execute_run` runs against the
`RunSink` trait for both, so sequencing, retries, fan-out, redaction, timeouts
and cancellation are already one code path (`src/worker/mod.rs:13`: *"only the
sink differs from an in-process run"*). The asymmetry is entirely in the
surrounding lifecycle:

| Concern    | Local (today)                              | Remote (today)              |
|------------|--------------------------------------------|-----------------------------|
| Dispatch   | push — `Engine::start` spawns a task        | pull — `claim_runs` (lease) |
| Lease      | none                                        | 30s lease + heartbeat       |
| Recovery   | `recover_interrupted` at startup            | lease reaper                |
| Retry      | would be separate                           | would be separate           |

## Goal

The server hosts an **in-process worker** that claims runs off a queue with a
lease, exactly like a remote worker. After that, dispatch, lease, reaper and
retry are **one mechanism**; local and remote differ only in transport
(in-process calls vs HTTP) and sink (`LocalSink` direct-write vs `RemoteSink`).

Decisions locked during brainstorming:

- **Scope:** unify the lifecycle, keep two sinks. The in-process worker writes
  straight to the authoritative DB via `LocalSink`; no self-serialization.
- **Pickup:** periodic poll (like the remote worker), with a visible `Queued`
  UI state — every run's normal first state, local or remote.
- **Capacity:** the in-process worker has a configurable capacity (default 8;
  `0` disables it → pure control plane). Bursts beyond capacity wait in
  `Queued`, same backpressure as a remote worker.
- **Retry:** in scope. Run-level retry-on-loss, whole-run from scratch,
  opt-in per flow.

## Design

### 1. In-process worker

A small new loop (`src/engine/local_worker.rs` or similar), spawned from `main`
when capacity > 0. Shape mirrors the remote worker but talks to `Engine`/`Db`
directly:

```
loop every POLL_INTERVAL (or until shutdown):
  free    = capacity − in_flight
  claimed = db.claim_runs(LOCAL_WORKER_ID, ["local"], free, LEASE_SECS)
  for run in claimed:
     register run in the engine active map (cancel token + event channel)
     spawn: execute_run(engine, run, cancel, LocalSink)
  db.renew_leases(LOCAL_WORKER_ID, in_flight_ids, LEASE_SECS)   // heartbeat
```

- Reuses the existing `claim_runs` / `renew_leases` / `execute_run` /
  `LocalSink` — nothing new in the execution or lease protocol.
- `worker_id = "local"` (a stable constant). The reaper and claim treat it
  identically to any remote worker.
- Cancellation stays direct: runs are registered in the engine's active map
  with a `CancellationToken`, so `Engine::cancel` works unchanged; no HTTP
  cancel signal needed in-process.
- A hung-but-alive in-process run keeps being heartbeated (heartbeat is
  independent of the run task), so the reaper never reaps a live run —
  identical to a remote worker running a stuck task. Stuck plugins are already
  bounded by `execute_run`'s per-attempt timeout.

### 2. Dispatch: everyone just enqueues

- Remove `Engine::start`'s push path and its `queue == "local"` special-case
  (`src/engine/mod.rs:303-353`).
- `Engine::create_and_start` becomes `create_run` (insert `queued` only).
- The scheduler's `RunLauncher` (`src/main.rs:145-154`) and the API create path
  stop calling `start()` — they only insert a `queued` row. This deletes the
  "scheduler failed to start run" branch and makes scheduler / API / remote
  creation identical: everyone enqueues, a worker claims.
- **Queued UI state:** `queued` already exists in the schema and data; ensure
  the run list/detail renders it as a healthy, distinct state (it is now the
  normal first state of every run), not an ambiguous/pending-error look.

### 3. Unified recovery

Two recovery paths collapse into one once in-process runs carry leases:

- **Reaper always runs.** Drop the `worker_tokens`-gated spawn
  (`src/main.rs:271-277`) — a server with an in-process worker always needs it.
- **Startup = one reap pass.** After a crash every lease is stale, so a single
  `reap_expired_leases` handles both a crashed in-process run and a remote run
  whose worker vanished while the server was down. **`recover_interrupted` is
  deleted.**
- **Ordering:** the startup reap must run *before* the in-process worker's
  first claim, so a crashed run is requeued (fresh `attempt`, cleared lease)
  before it can be re-claimed.
- A `queued`-at-crash run needs no handling — it is still `queued` and gets
  claimed normally.

### 4. Retry-on-loss

Whole-run, from scratch, opt-in per flow. Retry is a branch inside reap — the
single point where a lost run is resolved — so it is uniform for local and
remote.

**Config.** New field on `FlowDefinition` (`src/model/flow.rs:22`):

```yaml
on_worker_loss:
  max_attempts: 3      # absent or 1 = today's behavior (fail, no retry)
```

Opting in *is* the idempotency assertion (a flow that already POSTed before the
drop will POST again on retry). Validation mirrors `RetryPolicy`
(`max_attempts` 1–20). `skip_serializing_if` keeps existing flows
byte-identical on round-trip.

**Attempt tracking.** `runs` migration adds `attempt INTEGER NOT NULL DEFAULT
0`; existing rows backfill to 0.

**Reap decision** (per expired lease):

- If `attempt + 1 < max_attempts` → **requeue**: `status='queued'`,
  `attempt += 1`, clear `worker_id` / `lease_expires_at` / `last_seq` /
  `started_at` / `error`, delete the prior `task_runs` + `task_run_items`
  (clean slate — a fresh attempt re-creates them), emit a `requeued` event.
- Else → **fail** as today (`"worker lost (lease expired)"`).

**Defaults locked:** clear failed-attempt task history (the `attempt` counter +
a log line record the retry); pin `flow_rev` across attempts (a retry re-runs
the same code, not a newer revision the run never saw).

**Cancellation wins:** a canceled run is terminal, which the reaper never
touches, so cancel during a retry cycle stops it — no requeue.

### 5. Ownership fence (retry correctness)

Retry reintroduces a hazard the shipped terminal-state fence does not cover,
because a requeued run is deliberately non-terminal again:

> Remote worker drops → reaper **requeues** (`last_seq` → 0, `worker_id`
> cleared) → a **new** worker claims and starts attempt 2 → the **old** worker
> reconnects and flushes buffered updates with seqs 10, 11… → `bump_seq`
> accepts them (10 > 0) → they corrupt attempt 2's row.

Fix: an **ownership check on the update path.** The `updates` handler
(`src/api/worker.rs:125`) already carries `body.worker_id`; thread it into
`apply_remote_update` (or check once per batch) and reject when the reporting
`worker_id` ≠ the run's current `runs.worker_id`, responding `canceled: true`
so the stale worker stops.

The two fences are complementary — both are needed:

| Scenario                          | run state    | worker_id           | Caught by       |
|-----------------------------------|--------------|---------------------|-----------------|
| Reaped → failed (no retry)        | terminal     | still old worker    | terminal fence  |
| Reaped → requeued → reclaimed     | non-terminal | changed to new one  | ownership fence |

In-process runs need no fence here (they use `LocalSink`, no update path; a
crashed in-process worker is gone, not racing).

## Edge cases

- **`capacity: 0`** → no in-process worker (pure control plane); reaper still
  runs, remote workers unaffected.
- **Shared `local` queue** → `claim_runs` is atomic, so a remote worker *may*
  also serve `local` with no double-lease risk — a free consequence (local work
  can be offloaded).
- **Orphan queue** → a run routed to a queue with no worker sits `queued`
  forever, same as today.
- **Migration backfill** → existing runs get `attempt = 0`.

## Testing

Builds on the tempdir-`Db` + `Engine` harness added with the terminal fence.

- **In-process worker:** claim → execute → `success`; capacity backpressure
  (N+1th run waits `queued`); heartbeat keeps a live run from being reaped.
- **Unified recovery:** a `running` row with a stale lease at startup →
  failed (retry off) or requeued (retry on); a `queued`-at-crash row untouched.
- **Retry:** requeue while attempts remain; fail when exhausted; `flow_rev`
  pinned; task rows cleared on requeue; cancel-during-retry doesn't requeue;
  non-opted-in flow hard-fails.
- **Ownership fence:** after requeue + reclaim by a new worker, stale
  old-worker `updates` rejected; plus the existing terminal-fence test.

## Net effect

One enqueue path, one claim/lease mechanism, one recovery story, one retry
implementation. Local and remote differ only in transport and sink.
