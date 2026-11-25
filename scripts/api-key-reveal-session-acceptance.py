#!/usr/bin/env python3
"""Live acceptance — the key UI ruling (plan §6.2 / §8) on WorkflowSwift.

docs/fleet-per-account-keys-and-mcp-2026-11-25.md §6.2 (David, 2026-11-25): the key is private by
default (only the prefix is ever listed) and **View is ONE CLICK** — a valid session is enough, no
password re-prompt — while every reveal is still audited. David's own words: "they should be able to
see the key ... a click to view ... and also a copy button".

The same ruling settles the security half: this app's auth middleware accepts an API key as well as a
session, so `reveal` classifies the credential (`Claims::via_api_key`) and refuses a KEY with 403.
Otherwise a read-only key could obtain the account's read+write key.

Two handlers are proved, because the app has two key tables and two reveal routes:
  * programmatic keys  — POST /api/personal-api-keys/<id>/reveal   (`api_keys`)
  * integration keys   — POST /api/v1/user-keys/<id>/reveal         (`user_api_keys`)

Checks (one PASS/FAIL line each, exit 1 on any FAIL):
  1.  signup probe A minted exactly ONE key; the list is prefix-only (no plaintext)
  1b. the integration key signup itself provisioned reveals in one click (minted sealed)
  2.  programmatic reveal with the session and NO BODY AT ALL      -> 200 + the full key
  3.  the legacy {"password": "…"} body still answers              -> 200 (old clients keep working)
  4.  every programmatic reveal wrote an `api_key_reveals` row (>= 2 by now)
  5.  reveal with NO credential                                    -> 401
  6.  reveal with the account's API KEY as bearer                  -> 403 (session-only)
  7.  probe B's key id with probe A's session                       -> 404
  8.  the revealed value is a working credential (GET keys -> 200)
  9.  revoke the key, then reveal it with the session               -> 404
  10. integration key: mint with the session, reveal with NO BODY   -> 200 + the full key
  11. integration key: reveal with the API KEY as bearer            -> 403
  12. integration key: reveal with NO credential                    -> 401
  13. teardown: both probe accounts erased, residue 0

Never prints a full key — prefixes only.

Usage: python3 scripts/api-key-reveal-session-acceptance.py [base_url]
"""
import json
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8085").rstrip("/")
CONTAINER = "workflowswift"
DB = "workflowswift"
HARNESS = "t_18a8fa3c"
UA = "workflowswift-key-reveal-session-acceptance/1.0"

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, api_key=None, extra_headers=None, raw_body=False):
    url = BASE + path
    if body is None:
        data = b"" if raw_body else None
    else:
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", UA)
    if token:
        req.add_header("Authorization", "Bearer " + token)
    if api_key:
        req.add_header("Authorization", "Bearer " + api_key)
    for k, v in (extra_headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode(errors="replace")
            try:
                return r.status, (json.loads(raw) if raw else {})
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode(errors="replace")
        try:
            return e.code, (json.loads(raw) if raw else {})
        except json.JSONDecodeError:
            return e.code, raw
    except urllib.error.URLError as e:
        raise SystemExit(f"cannot reach {url}: {e}")


def _dsn():
    """The app's DATABASE_URL from its container env (never printed)."""
    r = subprocess.run(
        ["docker", "inspect", CONTAINER, "--format",
         "{{range .Config.Env}}{{println .}}{{end}}"],
        capture_output=True, text=True, timeout=30,
    )
    if r.returncode != 0:
        raise SystemExit(f"docker inspect failed: {r.stderr.strip()[:200]}")
    for line in r.stdout.splitlines():
        if line.startswith("DATABASE_URL="):
            return line.split("=", 1)[1].strip()
    raise SystemExit(f"no DATABASE_URL in {CONTAINER} env")


_DSN = _dsn()
_U = urllib.parse.urlparse(_DSN)


def sql(query):
    r = subprocess.run(
        ["docker", "exec", "-i", "-e", f"PGPASSWORD={urllib.parse.unquote(_U.password or '')}",
         "swift-postgres", "psql", "-U", urllib.parse.unquote(_U.username or "swift"),
         "-d", DB, "-tAF", "|", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise SystemExit(f"SQL failed: {r.stderr.strip()[:400]}\n--- {query[:200]}")
    return [line.split("|") for line in r.stdout.splitlines() if line.strip()]


def scalar(query):
    rows = sql(query)
    return rows[0][0] if rows else None


def register_probe(tag, letter):
    email = f"probe-keyview-{letter}-{tag}@example.invalid"
    password = "ProbeKeyView!2026" + tag[-4:]
    s, b = http("POST", "/api/v1/auth/register", {
        "name": f"KeyView Probe {letter}",
        "email": email,
        "password": password,
        "account_name": f"KeyView Probe {letter} {tag}",
    }, extra_headers={"X-Swift-Harness": HARNESS})
    if s not in (200, 201):
        raise SystemExit(f"probe signup failed: {s} {b}")
    b = b if isinstance(b, dict) else {}
    account = {
        "email": email,
        "password": password,
        "token": b.get("access_token"),
        "aid": (b.get("account") or {}).get("id"),
        "uid": (b.get("user") or {}).get("id"),
    }
    # A key is REFUSED by the middleware with 402 unless the account's plan carries `api_access`
    # (`api_key_auth::authenticate` enforces it before any handler runs), so put the probe on the
    # cheapest tier that has it — the same premise the P1c harness uses.
    if account["aid"]:
        plan = scalar("SELECT id FROM plan_tiers WHERE has_api_access = true AND is_active = true "
                      "ORDER BY COALESCE(price_monthly,0) ASC LIMIT 1")
        if plan:
            sql(f"UPDATE accounts SET plan_id = '{plan}' WHERE id = '{account['aid']}';"
                # An ACTIVE account_plans row outranks accounts.plan_id in features::resolve_plan_id.
                f"DELETE FROM account_plans WHERE aid = '{account['aid']}' AND status = 'active';")
    return account


def list_keys(p):
    s, b = http("GET", "/api/personal-api-keys", token=p["token"])
    keys = (b.get("api_keys") or b.get("keys") or []) if isinstance(b, dict) else []
    return s, keys


def erase(p):
    """The app's own erasure door is the supported teardown (password + typed word)."""
    s, _ = http("POST", "/api/v1/auth/erase-data",
                {"password": p["password"], "confirm": "DELETE"}, token=p["token"])
    return s


def main():
    tag = time.strftime("%Y%m%d-%H%M%S")
    a = register_probe(tag, "a")
    b = register_probe(tag, "b")
    if not a["token"] or not a["aid"] or not b["token"]:
        raise SystemExit("probe signup returned no token/account id")
    print(f"probe A {a['aid']}  probe B {b['aid']}")

    # ── 1. signup minted exactly one key; the list never carries the secret ─────────────────────
    s, keys_a = list_keys(a)
    key = keys_a[0] if keys_a and isinstance(keys_a[0], dict) else None
    check("1 signup minted ONE key and the list is prefix-only",
          s == 200 and len(keys_a) == 1 and key is not None
          and "key" not in key and "token" not in key and bool(key.get("prefix")),
          f"HTTP {s} keys={len(keys_a)} prefix={key.get('prefix') if key else None}")
    if not key:
        raise SystemExit("no key to reveal — cannot continue")

    # ── 1b. the keys signup itself provisioned are readable too (each is minted sealed) ─────────
    s, ulist = http("GET", "/api/v1/user-keys", token=a["token"])
    ukeys = (ulist.get("keys") or []) if isinstance(ulist, dict) else []
    prov = next((k for k in ukeys
                 if isinstance(k, dict) and str(k.get("prefix", "")).startswith("whsec_")), None)
    s2, ubody = (http("POST", f"/api/v1/user-keys/{prov['id']}/reveal", token=a["token"], raw_body=True)
                 if prov else (0, {}))
    ubody = ubody if isinstance(ubody, dict) else {}
    check("1b signup's own integration key is revealable in one click",
          s == 200 and prov is not None and s2 == 200
          and str(ubody.get("key", "")).startswith("whsec_"),
          f"list HTTP {s} keys={len(ukeys)} reveal {s2}")

    # ── 2. one click: session + NO BODY ─────────────────────────────────────────────────────────
    s, body = http("POST", f"/api/personal-api-keys/{key['id']}/reveal", token=a["token"], raw_body=True)
    full = body.get("key") if isinstance(body, dict) else None
    check("2 reveal with the session and no body -> 200 + the full key",
          s == 200 and isinstance(full, str) and full.startswith("workflowswift_"),
          f"HTTP {s} prefix={body.get('prefix') if isinstance(body, dict) else None}")

    # ── 3. old clients that still send a password keep working (the body is ignored) ────────────
    s, body = http("POST", f"/api/personal-api-keys/{key['id']}/reveal",
                   {"password": "ignored-legacy-field"}, token=a["token"])
    check("3 legacy {\"password\":…} body still answers 200", s == 200, f"HTTP {s}")

    # ── 4. the audit row ────────────────────────────────────────────────────────────────────────
    n = int(scalar("SELECT COUNT(*) FROM api_key_reveals "
                   f"WHERE key_id = '{key['id']}'") or 0)
    check("4 every reveal wrote an api_key_reveals row", n >= 2, f"{n} row(s)")

    # ── 5. anonymous ────────────────────────────────────────────────────────────────────────────
    s, _ = http("POST", f"/api/personal-api-keys/{key['id']}/reveal", raw_body=True)
    check("5 reveal with no credential -> 401", s == 401, f"HTTP {s}")

    # ── 6. an API KEY cannot view keys ──────────────────────────────────────────────────────────
    s, body = http("POST", f"/api/personal-api-keys/{key['id']}/reveal", api_key=full, raw_body=True)
    check("6 reveal with the account's API key -> 403 (session-only)",
          s == 403, f"HTTP {s} {str(body)[:120]}")

    # ── 7. another workspace's key id is not found ──────────────────────────────────────────────
    _, keys_b = list_keys(b)
    foreign = keys_b[0]["id"] if keys_b else None
    s, _ = http("POST", f"/api/personal-api-keys/{foreign}/reveal", token=a["token"], raw_body=True)
    check("7 probe B's key id with probe A's session -> 404", s == 404, f"HTTP {s}")

    # ── 8. the value revealed is a working credential ───────────────────────────────────────────
    s, _ = http("GET", "/api/personal-api-keys", api_key=full)
    check("8 the revealed key authenticates (GET keys -> 200)", s == 200, f"HTTP {s}")

    # ── 10. the integration-key twin (user_api_keys) ────────────────────────────────────────────
    s, minted = http("POST", "/api/v1/user-keys/generate",
                     {"key_type": "webhook_secret", "label": "probe-keyview"}, token=a["token"])
    minted = minted if isinstance(minted, dict) else {}
    ukey_id, ukey_full = minted.get("id"), minted.get("key")
    check("10a integration key minted with the session", s == 201 and bool(ukey_id) and bool(ukey_full),
          f"HTTP {s} id={str(ukey_id)[:8]}…")
    s, body = http("POST", f"/api/v1/user-keys/{ukey_id}/reveal", token=a["token"], raw_body=True)
    ufull = body.get("key") if isinstance(body, dict) else None
    check("10 integration reveal with the session and no body -> 200",
          s == 200 and isinstance(ufull, str) and ufull.startswith("whsec_"),
          f"HTTP {s}")

    # ── 11/12. the same refusal on the integration twin ─────────────────────────────────────────
    s, _ = http("POST", f"/api/v1/user-keys/{ukey_id}/reveal", api_key=full, raw_body=True)
    check("11 integration reveal with the API key -> 403", s == 403, f"HTTP {s}")
    s, _ = http("POST", f"/api/v1/user-keys/{ukey_id}/reveal", raw_body=True)
    check("12 integration reveal with no credential -> 401", s == 401, f"HTTP {s}")

    # ── 9. a revoked key is gone, not shown ─────────────────────────────────────────────────────
    s, _ = http("DELETE", f"/api/v1/user-keys/{ukey_id}", token=a["token"])
    s2, _ = http("POST", f"/api/v1/user-keys/{ukey_id}/reveal", token=a["token"], raw_body=True)
    check("9 revoke the integration key, then reveal -> 404",
          s == 200 and s2 == 404, f"revoke {s} reveal {s2}")

    # ── 13. teardown ────────────────────────────────────────────────────────────────────────────
    ea, eb = erase(a), erase(b)
    for p in (a, b):
        aid = p["aid"]
        sql(f"DELETE FROM api_key_reveals WHERE aid = '{aid}';"
            f"DELETE FROM api_key_events WHERE owner_id = '{aid}';"
            f"DELETE FROM erasure_billing_retention WHERE erased_tenant_id = '{aid}';"
            f"DELETE FROM account_erasures WHERE erased_tenant_id = '{aid}';")
    residue = int(scalar(f"SELECT COUNT(*) FROM accounts WHERE probe_harness = '{HARNESS}'") or 0)
    rows = int(scalar("SELECT COUNT(*) FROM accounts "
                      f"WHERE id IN ('{a['aid']}','{b['aid']}')") or 0)
    check("13 both probe workspaces erased, residue 0",
          ea == 200 and eb == 200 and residue == 0 and rows == 0,
          f"erase a={ea} b={eb} residue={residue} rows={rows}")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
