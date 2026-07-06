#!/usr/bin/env bash
# Orchestrator demo: starts a tiny mock API, imports examples/demo-flow.yaml,
# triggers a run, and follows it to completion.
#
# Prerequisite: an Orchestrator server running (default http://127.0.0.1:4400;
# override with ORCH_URL). Requires python3 and curl.
set -euo pipefail

ORCH_URL="${ORCH_URL:-http://127.0.0.1:4400}"
MOCK_HOST=127.0.0.1
MOCK_PORT=4599
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FLOW_FILE="$ROOT/examples/demo-flow.yaml"
FLOW_ID=demo_civic_minutes

# ---------------------------------------------------------------------------
# Mock API on 127.0.0.1:4599
#   GET  /municipalities   -> {"ids":[1..12]}
#   GET  /municipality/N   -> {"id":N,"name":"Town N"}   (3, 7, 11 return 404)
#   POST /report           -> {"ok":true}  (logs what it received)
# ---------------------------------------------------------------------------
python3 - "$MOCK_HOST" "$MOCK_PORT" <<'PY' &
import json, re, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST, PORT = sys.argv[1], int(sys.argv[2])
MISSING = {3, 7, 11}  # these ids 404 to demonstrate on_error: continue

class Handler(BaseHTTPRequestHandler):
    def _json(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/municipalities":
            return self._json(200, {"ids": list(range(1, 13))})
        m = re.fullmatch(r"/municipality/(\d+)", self.path)
        if m:
            n = int(m.group(1))
            if n in MISSING:
                return self._json(404, {"error": f"municipality {n} not found"})
            return self._json(200, {"id": n, "name": f"Town {n}"})
        return self._json(404, {"error": "no such path"})

    def do_POST(self):
        if self.path == "/report":
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length)
            try:
                data = json.loads(raw)
                results = data.get("results") or []
                fetched = sum(1 for r in results if r is not None)
                dropped = sum(1 for r in results if r is None)
                print(
                    f"[mock] report received: {len(results)} results "
                    f"({fetched} fetched, {dropped} dropped as null), "
                    f"regions={data.get('regions')}, dry_run={data.get('dry_run')}",
                    flush=True,
                )
            except Exception:
                print("[mock] report received (unparseable body)", flush=True)
            return self._json(200, {"ok": True})
        return self._json(404, {"error": "no such path"})

    def log_message(self, fmt, *args):  # silence per-request access logs
        pass

print(f"[mock] listening on http://{HOST}:{PORT}", flush=True)
ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()
PY
MOCK_PID=$!
cleanup() { kill "$MOCK_PID" 2>/dev/null || true; }
trap cleanup EXIT

# Wait for the mock to accept requests.
for _ in $(seq 1 50); do
  curl -sf "http://$MOCK_HOST:$MOCK_PORT/municipalities" >/dev/null 2>&1 && break
  sleep 0.1
done

# Wait for Orchestrator.
echo "waiting for Orchestrator at $ORCH_URL ..."
ok=""
for _ in $(seq 1 100); do
  if curl -sf "$ORCH_URL/api/health" >/dev/null 2>&1; then ok=1; break; fi
  sleep 0.3
done
if [ -z "$ok" ]; then
  echo "error: Orchestrator is not reachable at $ORCH_URL" >&2
  echo "start it first:  ./target/release/orchestrator serve" >&2
  exit 1
fi

# Import the demo flow (re-running re-imports as a new revision — fine).
echo "importing $FLOW_FILE ..."
import_response=$(curl -sS -X POST "$ORCH_URL/api/flows/import" \
  --data-binary @"$FLOW_FILE")
if ! printf '%s' "$import_response" | grep -q "\"$FLOW_ID\""; then
  echo "error: import failed: $import_response" >&2
  exit 1
fi
echo "imported flow $FLOW_ID"

# Trigger a run.
run_response=$(curl -sS -X POST "$ORCH_URL/api/flows/$FLOW_ID/run" \
  -H 'content-type: application/json' \
  -d '{"inputs": {"regions": ["ON", "QC"], "dry_run": false}}')
RUN_ID=$(printf '%s' "$run_response" | python3 -c \
  'import sys, json; print(json.load(sys.stdin)["run_id"])' 2>/dev/null) || {
  echo "error: run trigger failed: $run_response" >&2
  exit 1
}

echo
echo "run $RUN_ID started — open $ORCH_URL/runs/$RUN_ID to watch it live"
echo

# Follow the run to completion (keep the mock alive until it finishes).
STATUS=queued
for _ in $(seq 1 120); do
  STATUS=$(curl -sf "$ORCH_URL/api/runs/$RUN_ID" | python3 -c \
    'import sys, json; print(json.load(sys.stdin)["run"]["status"])')
  case "$STATUS" in
    queued|running) sleep 1 ;;
    *) break ;;
  esac
done

echo "run $RUN_ID finished: $STATUS"
curl -sf "$ORCH_URL/api/runs/$RUN_ID" | python3 -c '
import sys, json
d = json.load(sys.stdin)
for t in d["tasks"]:
    print("  task {:<10} {}".format(t["task_id"], t["status"]))
for task_id, agg in d.get("fanout", {}).items():
    print(
        "  fan-out {}: {} items — {} success, {} dropped, {} failed".format(
            task_id, agg["total"], agg["success"], agg["dropped"], agg["failed"]
        )
    )
'
echo
echo "details: $ORCH_URL/runs/$RUN_ID"
[ "$STATUS" = "success" ]
