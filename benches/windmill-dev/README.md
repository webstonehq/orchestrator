# Orchestrator benchmarks

A faithful port of the [Windmill "competitors"
benchmark](https://github.com/windmill-labs/windmill-benchmarks/tree/main/competitors)
(Windmill vs. Airflow, Prefect, Temporal, Kestra) to Orchestrator, so its
numbers can be compared against the published ones.

## What it measures

Two workflow shapes built from the same naive Fibonacci function the Windmill
benchmark uses:

| Flow       | Shape                                | Workload  | Isolates |
| ---------- | ------------------------------------ | --------- | -------- |
| `bench_10` | 10 sequential **long-running** tasks | `fibo(33)`| per-task execution cost |
| `bench_40` | 40 sequential **lightweight** tasks  | `fibo(10)`| orchestration overhead  |

```python
def fibo(n):
    if n <= 1:
        return n
    return fibo(n - 1) + fibo(n - 2)
```

Tasks run **strictly sequentially**. Orchestrator runs a flow's tasks in
definition order, so — unlike Kestra's `ForEach` with `concurrencyLimit: 1` —
listing N tasks is all that's needed.

The workload is a **`bench.fibo` plugin bundle**: a Python oneshot plugin
(`plugins/fibo/`) running the *identical* `fibo()`. Oneshot means the engine
spawns it fresh per task — the same model as Kestra's `Process` task runner —
so `fibo(33)` costs the same wall-clock across engines and `bench_40` measures
Orchestrator's real per-task spawn + dispatch overhead.

## Metrics

`analyze.py` pulls per-task timings from the run API and reports the same
decomposition the Windmill analysis does, as a percentage of total wall-clock:

- **Execution** — `finished_at − started_at`, the fibo work itself.
- **Assignment** — `started_at − created_at`, per-task queue/dispatch latency.
- **Transition** — gap between one task finishing and the next being created.
- **Overhead** — `total − execution`, everything that isn't the work.

> **In-process caveat.** Orchestrator executes a `local`-queue run's tasks
> in-process and sequentially — there is no distributed job queue for a task to
> sit in — so **assignment time is ~0** and the meaningful figure is
> **overhead**. This differs from Windmill/Temporal/Hatchet, where "sequential
> tasks" means enqueue→dequeue round-trips through a database; a like-for-like
> total wall-clock is still directly comparable. (Assignment time becomes
> non-zero when a flow is routed to a BYOW worker queue.)

The per-task `created_at` timestamp this relies on is recorded in the
`task_runs` table (added to align with the Windmill schema).

## Running it

Requires Docker. The published Orchestrator image is `debian:bookworm-slim`
with no interpreter, so `Dockerfile` adds `python3` and the fibo bundle on top
of it — mirroring the benchmark's "spin up via docker-compose" methodology.

```sh
# 1. (Re)generate the two flow YAMLs — already committed, only needed if edited.
python3 gen_flows.py

# 2. Build + start Orchestrator with the fibo plugin installed.
docker compose up --build -d

# 3. Run each benchmark: imports the flow, triggers a run via the API, waits
#    for it to finish, then prints and writes the timing breakdown.
./run_bench.sh bench_10
./run_bench.sh bench_40

# 4. Tear down (‑v drops the SQLite volume for a clean next run).
docker compose down -v
```

Each run writes `stats-<run_id>.md` and `timing-<run_id>.json` (git-ignored).
To match the published setup, run it on an EC2 `t2.medium`.

Point the harness at an existing server instead of compose with
`ORCH_URL=http://host:port ./run_bench.sh bench_10` (that server must have the
fibo bundle in its `--plugins-dir`).

## Files

| Path                     | Purpose |
| ------------------------ | ------- |
| `plugins/fibo/`          | The `bench.fibo` plugin bundle (`plugin.json` + Python oneshot executable). |
| `gen_flows.py`           | Emits `flows/bench_10.yaml` and `flows/bench_40.yaml`. |
| `flows/*.yaml`           | The two benchmark flows (committed; regenerate with `gen_flows.py`). |
| `Dockerfile`             | Published image + `python3` + fibo bundle. |
| `docker-compose.yml`     | Spins the above up on `:4400`. |
| `run_bench.sh`           | Import → trigger → wait → analyze, for one flow. |
| `analyze.py`             | Timing decomposition from the run API. |
