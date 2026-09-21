#!/bin/bash
# build.sh — ONE source of truth for the Swift Market Intel extension.
#
#   source (unpacked)  : extensions/dist/swift-market-intel/
#   built artifact     : extensions/dist/swift-market-intel-extension.zip
#   served (nginx root) : /opt/swift/nginx/www/workflowswift/swift-market-intel-extension.zip
#
# 2026-09-21: before this script the same zip lived in FOUR places (repo root, www/,
# public/, extensions/dist) plus the nginx web root, and two of them were different
# builds (root/www = 37976B v1.1.0, public = 35227B v1.0.0). Never hand-roll a zip:
# run this, commit the unpacked source + the zip it writes, and let it push the
# served copy. Anything that needs to change, changes in dist/ first.
set -euo pipefail

SRC_DIR="/opt/swift/apps/WorkflowSwift/extensions/dist/swift-market-intel"
OUT_DIR="/opt/swift/apps/WorkflowSwift/extensions/dist"
OUT_ZIP="$OUT_DIR/swift-market-intel-extension.zip"
WEB_ROOT="/opt/swift/nginx/www/workflowswift"

[ -f "$SRC_DIR/manifest.json" ] || { echo "FATAL: no manifest.json in $SRC_DIR"; exit 1; }

# Deterministic-ish, clean zip: contents at the archive root (Chrome wants that),
# no macOS junk, fixed timestamps so the same source yields a stable hash.
python3 - "$SRC_DIR" "$OUT_ZIP" <<'PY'
import os, sys, zipfile
src, out = sys.argv[1], sys.argv[2]
files = []
for root, dirs, names in os.walk(src):
    dirs[:] = sorted(d for d in dirs if d not in ('__pycache__', '.git'))
    for n in sorted(names):
        if n.startswith('.') or n.endswith(('.zip', '.crx', '.map', '.swp', '~')):
            continue
        p = os.path.join(root, n)
        files.append((os.path.relpath(p, src).replace(os.sep, '/'), p))
files.sort()
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
    for arc, p in files:
        zi = zipfile.ZipInfo(arc, date_time=(2026, 9, 21, 0, 0, 0))
        zi.compress_type = zipfile.ZIP_DEFLATED
        zi.external_attr = 0o644 << 16
        with open(p, 'rb') as fh:
            z.writestr(zi, fh.read())
print(f"  {len(files)+1} entries -> {out}")
PY

# Cloudflare caches this path by default (verified 2026-09-21: cf-cache-status HIT,
# age 843s, still serving the Jul 31 build after the origin file was replaced). So we
# ALSO publish an immutable, version-named copy — that URL has never been requested
# and therefore can never be stale. Point the site/guides at the versioned name.
VER="$(python3 -c "import json;print(json.load(open('$SRC_DIR/manifest.json'))['version'])")"
VERSIONED="swift-market-intel-extension-$VER.zip"

cp "$OUT_ZIP" "$WEB_ROOT/$VERSIONED"
cp "$OUT_ZIP" "$WEB_ROOT/swift-market-intel-extension.zip"
echo "  served (versioned, cache-safe): $WEB_ROOT/$VERSIONED"
echo "  served (rolling alias)        : $WEB_ROOT/swift-market-intel-extension.zip"
echo -n "  sha256: "; sha256sum "$OUT_ZIP" | cut -d' ' -f1
echo -n "  version in manifest: $VER"
echo
echo -n "  api base in config.js: "; grep -o "'[^']*'" "$SRC_DIR/config.js" | tail -1
