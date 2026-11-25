#!/usr/bin/env python3
"""Live acceptance — fleet-standard mint surface `/api/personal-api-keys` + CoreSwift multi-key
rule (P1c, card t_29efde4c), on WorkflowSwift.

Proves, against the RUNNING container, that the canonical surface exists, is private, delegates to
the app's ONE credential implementation, and that an account may hold several live keys:

  1.  GET    /api/personal-api-keys            anonymous          -> 401
  2.  GET    /api/personal-api-keys            fixture key        -> 200, lists the fixture prefix
  3.  POST   /api/personal-api-keys {name}     fixture key        -> 201, plaintext (`key`) + prefix
                                                                     (the SECOND live key: multi-key)
  4.  POST   /mcp tools/list                   minted key         -> 200, result.tools non-empty
  5.  DELETE /api/personal-api-keys/<minted>   fixture key        -> 200
  6.  POST   /mcp tools/list                   revoked key        -> 401
  7.  DELETE /api/personal-api-keys/<foreign>  fixture key        -> 404
  8.  POST   /api/personal-api-keys/<id>/reveal wrong password    -> 401 (password gate holds)
  10. teardown: probe rows deleted, residue = 0 for probe_harness = 't_29efde4c'

Fixtures are written DIRECTLY into the database (a live mint would consume the app's key rule), each
marked `probe_harness = 't_29efde4c'`. A raw key is `workflowswift_<16 hex>`, `key_hash` is
sha256_hex(raw), `prefix` is the first 8 hex of that digest. The probe account is put on the cheapest
`api_access` tier (the free tier's `api_access` flag is OFF, which would otherwise refuse the key 402).

Never prints a full key. Prefixes (8 chars) only.

Usage: python3 scripts/personal-api-keys-acceptance.py [base_url]
Exit 0 = every check passed. 1 = at least one failed (printed).
"""
from __future__ import annotations

import hashlib
import json
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8085").rstrip("/")
CONTAINER = "workflowswift"
DB = "workflowswift"
PROBE = "t_29efde4c"
UA = "workflowswift-personal-api-keys-acceptance/1.0"

FAILED: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def pgpassword() -> str:
    """The DB password from the container env, without printing it."""
    r = subprocess.run(
        ["docker", "inspect", CONTAINER, "--format",
         "{{range .Config.Env}}{{println .}}{{end}}"],
        capture_output=True, text=True, timeout=30,
    )
    if r.returncode != 0:
        raise SystemExit(f"docker inspect failed: {r.stderr.strip()[:200]}")
    for line in r.stdout.splitlines():
        if line.startswith("DATABASE_URL=postgres://swift:"):
            pw = line[len("DATABASE_URL=postgres://swift:"):].split("@", 1)[0]
            return pw
    raise SystemExit("no DATABASE_URL in container env")


PGPASSWORD = pgpassword()


def sql(sql_text: str) -> str:
    """One statement (or `;`-joined batch) through the app's own Postgres; stdout only."""
    r = subprocess.run(
        ["docker", "exec", "-e", f"PGPASSWORD={PGPASSWORD}", "swift-postgres",
         "psql", "-U", "swift", "-d", DB, "-tA", "-v", "ON_ERROR_STOP=1", "-c", sql_text],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise SystemExit(f"SQL failed: {r.stderr.strip()[:400]}\n--- {sql_text[:200]}")
    return r.stdout.strip()


def scalar(sql_text: str) -> str:
    out = sql(sql_text)
    return out.splitlines()[0] if out else ""


def sha256_hex(raw: str) -> str:
    return hashlib.sha256(raw.encode()).hexdigest()


def http(method: str, path: str, body=None, api_key: str | None = None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", UA)
    if api_key:
        req.add_header("Authorization", "Bearer " + api_key)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode(errors="replace")
            try:
                return r.status, json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            return e.code, raw
    except urllib.error.URLError as e:
        raise SystemExit(f"cannot reach {url}: {e}")


def make_probe_account(tag: str, role: str) -> dict:
    """One probe account + user + api key, written directly in the DB (marked probe_harness)."""
    aid = scalar("SELECT gen_random_uuid()::text")
    uid = scalar("SELECT gen_random_uuid()::text")
    kid = scalar("SELECT gen_random_uuid()::text")
    plan = scalar(
        "SELECT id FROM plan_tiers WHERE has_api_access = true AND is_active = true "
        "ORDER BY COALESCE(price_monthly,0) ASC LIMIT 1"
    )
    raw_key = "workflowswift_" + secrets.token_hex(8)
    key_hash = sha256_hex(raw_key)
    prefix = key_hash[:8]

    email = f"probe-{tag}-{role}@example.invalid"
    sql(
        f"INSERT INTO accounts (id, name, account_slug, plan_id, probe_harness, is_active) "
        f"VALUES ('{aid}', 'Probe {tag} {role}', 'probe-{tag}-{role}', '{plan}', '{PROBE}', true);"
        f"INSERT INTO users (id, aid, email, password_hash, name, role, is_active) "
        f"VALUES ('{uid}', '{aid}', '{email}', 'x', 'Probe {tag} {role}', '{role}', true);"
        f"INSERT INTO api_keys (id, aid, user_id, name, key_hash, prefix, permissions, is_active) "
        f"VALUES ('{kid}', '{aid}', '{uid}', 'probe-fixture', '{key_hash}', '{prefix}', '[]', true);"
    )
    return {"aid": aid, "uid": uid, "key_id": kid, "raw_key": raw_key,
            "prefix": prefix, "email": email}


def main() -> int:
    tag = time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(2)

    account_a = make_probe_account(tag, "admin")
    account_b = make_probe_account(tag, "user")
    key_a = account_a["raw_key"]

    print(f"probe A prefix={account_a['prefix']} (account {account_a['aid']})")
    print(f"probe B prefix={account_b['prefix']} (account {account_b['aid']})")

    # ── 1. anonymous -> 401 ────────────────────────────────────────────────────────────────────
    s, b = http("GET", "/api/personal-api-keys")
    check("1 anonymous GET -> 401", s == 401, f"HTTP {s} {str(b)[:100]}")

    # ── 2. fixture key lists its own prefix ─────────────────────────────────────────────────────
    s, b = http("GET", "/api/personal-api-keys", api_key=key_a)
    listed = [k.get("prefix") for k in (b.get("api_keys") or [])] if isinstance(b, dict) else []
    check("2 fixture key lists -> 200 with its prefix",
          s == 200 and account_a["prefix"] in listed,
          f"HTTP {s} prefixes={listed}")

    # ── 3. mint the SECOND live key (proves multi-key) ─────────────────────────────────────────
    s, b = http("POST", "/api/personal-api-keys", {"name": "probe-mint"}, api_key=key_a)
    b = b if isinstance(b, dict) else {}
    minted_key = b.get("key")
    minted_prefix = b.get("prefix")
    check("3 mint -> 201 with plaintext and prefix",
          s == 201 and bool(minted_key) and bool(minted_prefix),
          f"HTTP {s} prefix={minted_prefix}")

    # The mint response does not carry the id, so resolve it from the list by prefix.
    s, b = http("GET", "/api/personal-api-keys", api_key=key_a)
    keys = (b.get("api_keys") or []) if isinstance(b, dict) else []
    minted = next((k for k in keys if k.get("prefix") == minted_prefix), None)
    minted_id = minted.get("id") if minted else None
    check("3b minted key is listed by prefix", bool(minted_id), f"id={str(minted_id)[:8]}…")

    # ── 4. the minted key answers tools/list on the MCP door ───────────────────────────────────
    if minted_key:
        s, b = http("POST", "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                    api_key=minted_key)
        tools = (b.get("result") or {}).get("tools") or [] if isinstance(b, dict) else []
        check("4 minted key tools/list -> 200 non-empty", s == 200 and bool(tools),
              f"HTTP {s} tools={len(tools)}")
    else:
        check("4 minted key tools/list -> 200 non-empty", False, "no minted key to present")

    # ── 5. revoke the minted key ────────────────────────────────────────────────────────────────
    if minted_id:
        s, b = http("DELETE", f"/api/personal-api-keys/{minted_id}", api_key=key_a)
        check("5 DELETE minted id -> 200", s == 200, f"HTTP {s} {str(b)[:100]}")
    else:
        check("5 DELETE minted id -> 200", False, "no minted id resolved")

    # ── 6. the revoked key is refused ───────────────────────────────────────────────────────────
    if minted_key:
        s, b = http("POST", "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                    api_key=minted_key)
        check("6 revoked key tools/list -> 401", s == 401, f"HTTP {s}")
    else:
        check("6 revoked key tools/list -> 401", False, "no minted key")

    # ── 7. a foreign id is 404, not 403 ─────────────────────────────────────────────────────────
    s, b = http("DELETE", f"/api/personal-api-keys/{account_b['key_id']}", api_key=key_a)
    check("7 foreign id DELETE -> 404", s == 404, f"HTTP {s} {str(b)[:100]}")

    # ── 8. reveal with a wrong password holds the gate (do not seed a known password) ──────────
    s, b = http("POST", f"/api/personal-api-keys/{account_a['key_id']}/reveal",
                {"password": "definitely-wrong"}, api_key=key_a)
    check("8 reveal wrong password -> 401/400", s in (400, 401), f"HTTP {s} {str(b)[:100]}")

    # ── 10. teardown: delete the probe accounts (cascade) and every non-cascaded audit row ─────
    # `api_key_events` and `api_key_reveals` carry no FK to accounts, so they are not cascade-deleted.
    sql(
        f"DELETE FROM api_key_reveals WHERE aid IN ('{account_a['aid']}','{account_b['aid']}');"
        f"DELETE FROM api_key_events WHERE owner_id IN ('{account_a['aid']}','{account_b['aid']}');"
        f"DELETE FROM accounts WHERE id IN ('{account_a['aid']}','{account_b['aid']}');"
    )

    residue_accounts = scalar(f"SELECT COUNT(*) FROM accounts WHERE probe_harness = '{PROBE}'")
    residue_keys = scalar(
        f"SELECT COUNT(*) FROM api_keys WHERE aid IN ('{account_a['aid']}','{account_b['aid']}')")
    residue_users = scalar(
        f"SELECT COUNT(*) FROM users WHERE aid IN ('{account_a['aid']}','{account_b['aid']}')")
    residue_events = scalar(
        f"SELECT COUNT(*) FROM api_key_events WHERE owner_id IN ('{account_a['aid']}','{account_b['aid']}')")
    residue_reveals = scalar(
        f"SELECT COUNT(*) FROM api_key_reveals WHERE aid IN ('{account_a['aid']}','{account_b['aid']}')")
    residue_total = int(residue_accounts or 0) + int(residue_keys or 0) + int(residue_users or 0) \
        + int(residue_events or 0) + int(residue_reveals or 0)
    check("10 probe residue = 0", residue_total == 0,
          f"accounts={residue_accounts} keys={residue_keys} users={residue_users} "
          f"events={residue_events} reveals={residue_reveals}")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
