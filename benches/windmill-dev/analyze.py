#!/usr/bin/env python3
"""Analyze per-task timings for one Orchestrator run and print the same
decomposition the Windmill benchmark reports — execution vs. assignment vs.
transition — sourced from Orchestrator's run API.

    python3 analyze.py <run_id> [--url http://127.0.0.1:4400]

Definitions (all relative to the run's wall-clock duration):
  execution  = finished_at - started_at    (the fibo work itself)
  assignment = started_at  - created_at     (queue/dispatch latency per task)
  transition = next.created_at - this.finished_at  (gap between tasks)
  overhead   = total - execution            (everything that isn't the work)

Note: Orchestrator executes a `local`-queue run's tasks in-process and
sequentially, so assignment latency is typically ~0 (there is no queue to sit
in) and the interesting number is `overhead` — per-task process-spawn plus
dispatch. On a worker queue (BYOW), assignment time becomes meaningful.

Writes stats-<run_id>.md and timing-<run_id>.json next to this script.
"""
import argparse
import json
import sys
import urllib.request
from datetime import datetime


def parse_ts(s):
    if not s:
        return None
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def fetch_run(url, run_id):
    with urllib.request.urlopen(f"{url}/api/runs/{run_id}") as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser(description="Analyze Orchestrator run timings")
    ap.add_argument("run_id")
    ap.add_argument("--url", default="http://127.0.0.1:4400")
    args = ap.parse_args()

    data = fetch_run(args.url.rstrip("/"), args.run_id)
    run = data["run"]
    tasks = [t for t in data["tasks"] if t.get("started_at") and t.get("finished_at")]
    tasks.sort(key=lambda t: t["started_at"])
    if not tasks:
        print("no completed tasks to analyze", file=sys.stderr)
        sys.exit(1)

    run_start = parse_ts(run["started_at"])
    run_end = parse_ts(run["finished_at"])
    total = (run_end - run_start).total_seconds()

    rows = []
    for t in tasks:
        created = parse_ts(t.get("created_at")) or parse_ts(t["started_at"])
        started = parse_ts(t["started_at"])
        finished = parse_ts(t["finished_at"])
        rows.append(
            {
                "task": t["task_id"],
                "created": (created - run_start).total_seconds(),
                "started": (started - run_start).total_seconds(),
                "completed": (finished - run_start).total_seconds(),
                "execution": (finished - started).total_seconds(),
                "assignment": (started - created).total_seconds(),
            }
        )

    n = len(rows)
    total_exec = sum(r["execution"] for r in rows)
    total_assign = sum(r["assignment"] for r in rows)
    transitions = [rows[i + 1]["created"] - rows[i]["completed"] for i in range(n - 1)]
    total_trans = sum(transitions)

    pct = lambda x: (x / total * 100) if total else 0.0
    exec_pct = pct(total_exec)
    assign_pct = pct(total_assign)
    trans_pct = pct(total_trans)
    overhead_pct = pct(total - total_exec)

    print(f"\nRun {args.run_id}  ({run['flow_id']})  —  {n} tasks")
    print(f"{'task':<8}{'exec(s)':>10}{'assign(s)':>12}{'start(s)':>10}{'end(s)':>10}")
    for r in rows:
        print(
            f"{r['task']:<8}{r['execution']:>10.3f}{r['assignment']:>12.4f}"
            f"{r['started']:>10.3f}{r['completed']:>10.3f}"
        )

    print("\nBreakdown (% of total wall-clock):")
    print(f"  Execution : {exec_pct:6.2f}%   ({total_exec:.3f}s)")
    print(f"  Assignment: {assign_pct:6.2f}%   ({total_assign:.4f}s)")
    print(f"  Transition: {trans_pct:6.2f}%   ({total_trans:.4f}s)")
    print(f"  Overhead  : {overhead_pct:6.2f}%   (total - execution)")
    print(f"\nTotal wall-clock: {total:.3f}s   ({total / n * 1000:.1f} ms/task avg)")

    with open(f"stats-{args.run_id}.md", "w") as f:
        f.write(f"# Run {args.run_id} — {run['flow_id']}\n\n")
        f.write(f"- Tasks: {n}\n")
        f.write(f"- Total wall-clock: {total:.3f}s ({total / n * 1000:.1f} ms/task avg)\n")
        f.write(f"- Execution: {total_exec:.3f}s ({exec_pct:.2f}%)\n")
        f.write(f"- Assignment: {total_assign:.4f}s ({assign_pct:.2f}%)\n")
        f.write(f"- Transition: {total_trans:.4f}s ({trans_pct:.2f}%)\n")
        f.write(f"- Orchestration overhead (total - execution): {overhead_pct:.2f}%\n")
    with open(f"timing-{args.run_id}.json", "w") as f:
        json.dump(
            {
                "run_id": args.run_id,
                "flow_id": run["flow_id"],
                "tasks": n,
                "total_s": round(total, 3),
                "execution_s": round(total_exec, 3),
                "assignment_s": round(total_assign, 4),
                "transition_s": round(total_trans, 4),
                "execution_pct": round(exec_pct, 2),
                "overhead_pct": round(overhead_pct, 2),
                "created_at": [round(r["created"], 3) for r in rows],
                "started_at": [round(r["started"], 3) for r in rows],
                "completed_at": [round(r["completed"], 3) for r in rows],
            },
            f,
            indent=2,
        )
    print(f"\nwrote stats-{args.run_id}.md and timing-{args.run_id}.json")


if __name__ == "__main__":
    main()
