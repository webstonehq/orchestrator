#!/usr/bin/env bash
# Run one benchmark flow end to end: import it, trigger a run via the API, wait
# for it to finish, then analyze per-task timings. Mirrors the Windmill
# benchmark howto (submit via API, then parse timings from the engine's store).
#
#   ./run_bench.sh bench_10
#   ./run_bench.sh bench_40
#
# Requires a running Orchestrator (default http://127.0.0.1:4400; override with
# ORCH_URL) with the fibo plugin installed — `docker compose up` does both.
set -euo pipefail

FLOW="${1:-}"
if [ -z "$FLOW" ]; then
  echo "usage: $0 <bench_10|bench_40>" >&2
  exit 2
fi
ORCH_URL="${ORCH_URL:-http://127.0.0.1:4400}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLOW_FILE="$HERE/flows/$FLOW.yaml"
[ -f "$FLOW_FILE" ] || { echo "no such flow file: $FLOW_FILE (run gen_flows.py first)" >&2; exit 2; }

# Wait for Orchestrator to accept requests.
for _ in $(seq 1 100); do
  curl -sf "$ORCH_URL/api/health" >/dev/null 2>&1 && break
  sleep 0.3
done
curl -sf "$ORCH_URL/api/health" >/dev/null 2>&1 || {
  echo "Orchestrator not reachable at $ORCH_URL — start it with 'docker compose up'" >&2
  exit 1
}

echo "importing $FLOW ..."
curl -sS -X POST "$ORCH_URL/api/flows/import" --data-binary @"$FLOW_FILE" >/dev/null

echo "triggering $FLOW ..."
run_response=$(curl -sS -X POST "$ORCH_URL/api/flows/$FLOW/run" \
  -H 'content-type: application/json' -d '{}')
RUN_ID=$(printf '%s' "$run_response" | python3 -c 'import sys, json; print(json.load(sys.stdin)["run_id"])') || {
  echo "run trigger failed: $run_response" >&2
  exit 1
}
echo "run $RUN_ID started — watch at $ORCH_URL/runs/$RUN_ID"

STATUS=queued
for _ in $(seq 1 1200); do
  STATUS=$(curl -sf "$ORCH_URL/api/runs/$RUN_ID" | python3 -c 'import sys, json; print(json.load(sys.stdin)["run"]["status"])')
  case "$STATUS" in
    queued|running) sleep 0.5 ;;
    *) break ;;
  esac
done
echo "run $RUN_ID finished: $STATUS"

python3 "$HERE/analyze.py" "$RUN_ID" --url "$ORCH_URL"
