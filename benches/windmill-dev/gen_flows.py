#!/usr/bin/env python3
"""Generate the two benchmark flows, mirroring the Windmill 'competitors' suite:

- bench_10: 10 sequential long-running tasks, fibo(33)
- bench_40: 40 sequential lightweight tasks, fibo(10)

Each task is a `bench.fibo` plugin task. Orchestrator runs a flow's tasks
sequentially in definition order, so — unlike Kestra's ForEach with
concurrencyLimit: 1 — no concurrency knob is needed; listing N tasks is enough.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "flows")


def gen(flow_id, name, description, count, n):
    lines = [
        f"id: {flow_id}",
        f"name: {name}",
        "namespace: bench",
        f'description: "{description}"',
        "inputs: []",
        "variables: []",
        "triggers: []",
        "tasks:",
    ]
    for i in range(count):
        lines.append(f"- id: t{i:02d}")
        lines.append("  type: bench.fibo")
        lines.append(f"  config: {{ n: {n} }}")
    return "\n".join(lines) + "\n"


FLOWS = [
    ("bench_10", "bench-10", "10 sequential long-running tasks: fibo(33)", 10, 33),
    ("bench_40", "bench-40", "40 sequential lightweight tasks: fibo(10)", 40, 10),
]


def main():
    os.makedirs(OUT, exist_ok=True)
    for flow_id, name, desc, count, n in FLOWS:
        path = os.path.join(OUT, f"{flow_id}.yaml")
        with open(path, "w") as f:
            f.write(gen(flow_id, name, desc, count, n))
        print(f"wrote {path} ({count} tasks, fibo({n}))")


if __name__ == "__main__":
    main()
