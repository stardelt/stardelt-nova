#!/usr/bin/env bash
# Verify issue #76: a non-admin can use /, /dashboards and /notebooks.
#
# Serves the built SPA + a demo backend through a proxy that
#   - answers /api/me as a non-admin (is_admin: false),
#   - answers 403 on the operator endpoints (/api/apps, /api/apps/:id, /api/workloads, /api/trino/cluster),
#   - passes everything else through, including the new /api/embeds.
#
#   ./render.sh <outdir>
set -euo pipefail
OUT="${1:-/tmp/opencode/embed/nonadmin}"
PORT="${PORT:-8097}"
BACKEND_PORT=8096
mkdir -p "$OUT"

WT="$(cd "$(dirname "$0")" && pwd)"
DIST="$WT/frontend/dist"
BIN="$WT/target/debug/stardelt-nova-backend"

cargo build --manifest-path "$WT/backend/Cargo.toml" >/dev/null 2>&1

NOVA_DEMO=1 NOVA_BIND_ADDR="127.0.0.1:$BACKEND_PORT" NOVA_STATIC_DIR="$DIST" \
  "$BIN" >"$OUT/backend.log" 2>&1 &
BPID=$!
trap 'kill $BPID $PPID_PROXY 2>/dev/null || true' EXIT

python3 - "$PORT" "$BACKEND_PORT" <<'PY' &
import http.server, json, re, sys, urllib.request
port, upstream = int(sys.argv[1]), int(sys.argv[2])
OPERATOR = re.compile(r'^/api/(apps|workloads|trino/cluster)')

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/api/me'):
            body = json.dumps({"user": "analyst", "auth_mode": "oidc", "demo": True,
                               "warehouse": "warehouse", "sub": "s", "groups": [],
                               "is_admin": False, "via": "session"}).encode()
            self.send_response(200); self.send_header('content-type', 'application/json')
            self.send_header('content-length', str(len(body))); self.end_headers()
            self.wfile.write(body); return
        if OPERATOR.match(self.path):
            body = json.dumps({"error": "forbidden", "detail": "this view is for platform administrators"}).encode()
            self.send_response(403); self.send_header('content-type', 'application/json')
            self.send_header('content-length', str(len(body))); self.end_headers()
            self.wfile.write(body); return
        with urllib.request.urlopen(f'http://127.0.0.1:{upstream}{self.path}') as r:
            data = r.read()
            self.send_response(r.status)
            self.send_header('content-type', r.headers.get('content-type', 'application/octet-stream'))
            self.send_header('content-length', str(len(data))); self.end_headers()
            self.wfile.write(data)
    def log_message(self, *a): pass

http.server.HTTPServer(('127.0.0.1', port), H).serve_forever()
PY
PPID_PROXY=$!
sleep 3

render() { # route -> file
  chromium --headless --disable-gpu --no-sandbox --virtual-time-budget=9000 \
    --dump-dom "http://127.0.0.1:$PORT$1" 2>/dev/null > "$OUT/$(echo "$1" | tr '/' '_').html"
  local f="$OUT/$(echo "$1" | tr '/' '_').html"
  printf '  %-14s %6s bytes  forbidden-mentions=%s  unreachable-mentions=%s  not-installed=%s\n' \
    "$1" "$(wc -c < "$f")" \
    "$(grep -ci 'forbidden' "$f" || true)" \
    "$(grep -ci 'unreachable' "$f" || true)" \
    "$(grep -ci 'not installed' "$f" || true)"
}

echo "== non-admin renders (proxy answers 403 to the operator endpoints) =="
for r in / /dashboards /notebooks; do render "$r"; done
echo
echo "== did the embed pages load the apps they need? =="
for f in "$OUT"/_dashboards.html "$OUT"/_notebooks.html; do
  printf '  %-22s %s\n' "$(basename "$f")" "$(grep -oiE 'JupyterHub, embedded|Apache Superset, embedded|running|not part of the core profile|not installed' "$f" | sort -u | tr '\n' ' ')"
done
echo
echo "== measured the operator endpoints through the proxy =="
for p in /api/embeds /api/apps /api/workloads /api/trino/cluster; do
  printf '  %-18s %s\n' "$p" "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT$p")"
done
echo
echo "== /api/embeds payload (what the pages consume) =="
curl -s "http://127.0.0.1:$PORT/api/embeds" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("  ", [(a["id"], a["status"], a["embed"]["state"]) for a in d])'
echo "artifacts: $OUT"
