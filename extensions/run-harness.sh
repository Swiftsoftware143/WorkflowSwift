#!/bin/bash
# run-harness.sh — behavioural gate for the Swift Market Intel MV3 pack.
#
# Loads the pack unpacked in real Chromium and asserts the things that were broken in
# 1.1.0 and the things that must not regress. Chromium and the harness are local; the
# only network use is the extension's own calls to workflowswift.com, which is the point
# (401 = reachable and auth-gated, 404 = wrong path).
#
# Exit 0 = pass. Non-zero = the assertion that failed, printed.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$HERE/ext-verify.js"
[ -f "$HARNESS" ] || HARNESS=/opt/swift/scripts/ext-verify.js
SOURCE="$HERE/dist/swift-market-intel"

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
cp -r "$SOURCE" "$W/ext"

echo "  loading $SOURCE unpacked in Chromium..."
NODE_PATH=/usr/lib/node_modules timeout 180 node "$HARNESS" "$W/ext" verify > "$W/out.json" || {
  echo "  FAIL  harness exited non-zero (see below)"; tail -5 "$W/out.json"; exit 1; }

PACK="$SOURCE" python3 - "$W/out.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
fails = []
def ck(ok, label, detail=""):
    print(f"  {'PASS' if ok else 'FAIL'}  {label}{'  ' + detail if detail else ''}")
    if not ok: fails.append(label)

ck(not d.get("fatal"), "harness completed", str(d.get("fatal") or ""))
ck(d.get("sw_target"), "service worker started")
import os
mf = json.load(open(os.path.join(os.environ["PACK"], "manifest.json")))
ck(d["ext_version"] == mf["version"],
   "harness loaded the version the manifest declares", d["ext_version"])
ck(not d["popup"].get("errors"), "popup renders with no page errors", str(d["popup"].get("errors")))
ck(d["content_script"].get("injected_event") == {"platform": "etsy"},
   "content script injects on a declared origin", str(d["content_script"].get("injected_event")))
ping = d["content_script"].get("ping") or {}
ck(isinstance(ping, dict) and ping.get("reply", {}).get("success") is True,
   "background -> content messaging answers", str(ping))
tc = d["options"].get("test_connection_with_invalid_token") or {}
ck(tc.get("cls") == "test-result error",
   "Test Connection reports a REJECTION for an invalid token", str(tc.get("text"))[:60])
ap = d.get("api_probes", {})
ck(ap.get("base") == "https://workflowswift.com/api/v1", "base URL is the /api/v1 path", str(ap.get("base")))
codes = {r["name"]: r["status"] for r in ap.get("results", [])}
reachable = [n for n, s in codes.items() if s == 401]
ck(len(reachable) >= 4, "endpoints reachable (401, not 404)", str(codes))
ck(codes.get("STATUS") == 401, "the Test Connection URL is reachable", str(codes.get("STATUS")))

print(f"\n{'FAIL' if fails else 'PASS'}: swift-market-intel behavioural gate"
      f"{' — ' + '; '.join(fails) if fails else ''}")
sys.exit(1 if fails else 0)
PY
