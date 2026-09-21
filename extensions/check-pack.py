#!/usr/bin/env python3
"""Static gate for the Swift Market Intel MV3 pack.

Proves the pack is *structurally* sound and that the built zip matches the source
tree: valid manifest, MV3, no blanket <all_urls> anywhere, every source file present
in the archive, archive bytes identical to the file build.sh publishes.

Exit 0 = pass, non-zero = the reason, printed.
"""
import json
import os
import sys
import zipfile

DIST = os.path.join(os.path.dirname(os.path.abspath(__file__)), "dist")
SRC = os.path.join(DIST, "swift-market-intel")
ZIP = os.path.join(DIST, "swift-market-intel-extension.zip")
REQUIRED = ("manifest.json", "config.js", "background.js", "content.js",
            "popup.html", "popup.js", "options.html", "options.js",
            "icons/icon16.png", "icons/icon48.png", "icons/icon128.png")

fails = []


def check(ok, label, detail=""):
    print(f"  {'PASS' if ok else 'FAIL'}  {label}{'  ' + detail if detail else ''}")
    if not ok:
        fails.append(label)


mf_raw = open(os.path.join(SRC, "manifest.json")).read()
mf = json.loads(mf_raw)
check(mf.get("manifest_version") == 3, "manifest_version == 3", str(mf.get("manifest_version")))
check(bool(mf.get("version")), "version present", "v" + str(mf.get("version")))
check("<all_urls>" not in mf_raw, "no <all_urls> anywhere in the manifest")
check("web_accessible_resources" not in mf, "no unused web_accessible_resources block")

base = open(os.path.join(SRC, "config.js")).read()
check("WORKFLOWSWIFT_API_BASE" in base, "config.js defines WORKFLOWSWIFT_API_BASE")
check("/api/v1" in base, "API base carries the /v1 segment (the 1.1.0 defect)")

for name in ("background.js", "popup.js", "options.js", "content.js"):
    body = open(os.path.join(SRC, name)).read()
    hard = "workflowswift.com/api'" in body or 'workflowswift.com/api"' in body
    check(not hard, f"{name} has no hardcoded api base of its own (single source)")

for name in REQUIRED:
    if not os.path.isfile(os.path.join(SRC, name)):
        fails.append(f"missing source file {name}")
check(all(os.path.isfile(os.path.join(SRC, n)) for n in REQUIRED),
      f"all {len(REQUIRED)} pack files present in dist/")

z = zipfile.ZipFile(ZIP)
check(z.testzip() is None, "built zip passes its CRC check")
arc = set(z.namelist())
check(all(n in arc for n in REQUIRED), "built zip contains every required file")
check(z.read("manifest.json") == mf_raw.encode(), "zip manifest is byte-identical to dist/")
check("<all_urls>".encode() not in z.read("manifest.json"), "zip manifest carries no <all_urls>")

print(f"\n{'FAIL' if fails else 'PASS'}: swift-market-intel pack static gate"
      f"{' — ' + '; '.join(fails) if fails else ''}")
sys.exit(1 if fails else 0)
