#!/usr/bin/env python3
"""Live acceptance — fleet account-closure/erasure standard §6, on WorkflowSwift (kanban t_9999fc8e).

Proves, against the RUNNING app, that the erasure door works and refuses exactly what it must:

  1. POST /api/v1/auth/close-account with a valid API key   -> 401, rows intact
  2. POST /api/v1/auth/erase-data   with a valid API key    -> 401, rows intact
  3. POST /api/v1/auth/erase-data   with no credential      -> 401
  4. POST /api/v1/auth/erase-data   wrong password + DELETE -> 401, rows intact
  5. POST /api/v1/auth/erase-data   right password + wrong word -> 400, rows intact
  6. successful erase -> 200; account gone, users gone, api_keys gone, old session 401,
     old key 401, exactly ONE account_erasures row, invoice KEPT with aid NULL
  7. a teammate (second user, same account, non-owner) -> 403 (ownership refusal)
  8. cleanup: no probe residue; prints the accounts census before/after

Legs that are static (route policy, handler locks, migration shape) are pinned as Rust unit tests
in `src/auth/route_policy.rs`; this script proves the behavioural legs against the live binary.

What it creates: TWO disposable accounts through the app's own signup door (a `@example.invalid`
address is auto-marked `probe_harness` by the signup). The first is erased for real; the second
carries a second user so the ownership refusal is measured against a real teammate. Each probe is
cleaned up by `erase_tenant`, and the accounts census is printed before and after so residue is
visible.

Usage: python3 scripts/erasure-acceptance.py [base_url]
"""
import hashlib
import json
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import unquote, urlparse

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8085").rstrip("/")
APP_ENV = Path("/etc/swift/env/workflowswift.env")

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, api_key=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
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


def _dsn():
    for line in APP_ENV.read_text().splitlines():
        if line.startswith("DATABASE_URL="):
            return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit(f"no DATABASE_URL in {APP_ENV}")


def sql(query):
    """One statement (or a `;`-joined batch) through the app's own Postgres, tuples only."""
    u = urlparse(_dsn())
    db = unquote(u.path).lstrip("/") or "workflowswift"
    r = subprocess.run(
        ["docker", "exec", "-i", "-e", f"PGPASSWORD={unquote(u.password or '')}",
         "swift-postgres", "psql", "-U", unquote(u.username or "swift"), "-d", db,
         "-tAF", "|", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise SystemExit(f"SQL failed: {r.stderr.strip()[:400]}\n--- {query[:200]}")
    return [line.split("|") for line in r.stdout.splitlines() if line.strip()]


def scalar(query):
    rows = sql(query)
    return rows[0][0] if rows else None


def count(table, column, aid):
    return int(scalar(f"SELECT COUNT(*) FROM {table} WHERE {column} = '{aid}'") or 0)


def sha256_hex(raw):
    return hashlib.sha256(raw.encode()).hexdigest()


def register_probe(tag):
    email = f"probe-erasure-{tag}@example.invalid"
    password = "ProbeErase!2026"
    status, body = http("POST", "/api/v1/auth/register", {
        "name": "Erasure Probe",
        "email": email,
        "password": password,
        "account_name": f"Erasure Probe {tag}",
        "account_slug": tag,
    })
    if status not in (200, 201):
        raise SystemExit(f"probe signup failed: {status} {body}")
    body = body if isinstance(body, dict) else {}
    return {
        "email": email,
        "password": password,
        "token": body["access_token"],
        "aid": body["account"]["id"],
        "user_id": body["user"]["id"],
    }


def mint_valid_api_key(p):
    """Grant API access, then rewrite the signup's auto-minted key so the script KNOWS its raw value.

    The signup lands the probe on the free tier, whose `api_access` flag is OFF, so the plan gate
    in `auth::api_key_auth::authenticate` would refuse the key 402 before it ever reached the
    handler's `via_api_key` check — which would make the "valid API key -> 401" leg test the plan
    gate instead of the refusal it exists to prove. Moving the account's plan to the cheapest
    tier with API access makes the key a REAL credential that authenticates, so the 401 comes from
    the handler's `via_api_key` refusal. The key row is reused (not re-minted) so the partial
    unique index `api_keys_one_active_per_aid` never objects.
    """
    sql(
        f"UPDATE account_plans SET plan_id = "
        f"(SELECT id FROM plan_tiers WHERE has_api_access = true AND is_active = true "
        f" ORDER BY COALESCE(price_monthly,0)+COALESCE(price_yearly,0) LIMIT 1) "
        f"WHERE aid = '{p['aid']}'"
    )
    raw_key = "workflowswift_" + secrets.token_hex(8)
    key_hash = sha256_hex(raw_key)
    prefix = key_hash[:8]
    sql(f"UPDATE api_keys SET key_hash = '{key_hash}', prefix = '{prefix}', is_active = true "
        f"WHERE aid = '{p['aid']}'")
    return raw_key


def seed_rows(p):
    """An invoice (the billing document that must SURVIVE, detached) and a workflow (a child row
    that must be DESTROYED), so both halves of the erasure contract are measured against real rows."""
    plan_id = scalar(f"SELECT plan_id FROM account_plans WHERE aid = '{p['aid']}' LIMIT 1")
    if not plan_id:
        raise SystemExit("probe account has no account_plans row to source a plan_id from")
    # The invoice row's plan_id references plan_tiers (NO ACTION), so it must name a real tier.
    sql(
        f"INSERT INTO invoices (id, aid, plan_id, amount, status, created_at) "
        f"VALUES (gen_random_uuid(), '{p['aid']}', '{plan_id}', 19.00, 'paid', NOW());"
        # A workflow row so "rows gone" is a real measurement, not an empty-table shrug.
        f"INSERT INTO workflows (id, aid, name, created_at, updated_at) "
        f"VALUES (gen_random_uuid(), '{p['aid']}', 'Erasure Probe Workflow', NOW(), NOW());"
    )


def snapshot(p):
    return {
        "accounts": count("accounts", "id", p["aid"]),
        "users": count("users", "aid", p["aid"]),
        "api_keys": count("api_keys", "aid", p["aid"]),
        "invoices": count("invoices", "aid", p["aid"]),
        "account_plans": count("account_plans", "aid", p["aid"]),
        "workflows": count("workflows", "aid", p["aid"]),
    }


def login(email, password):
    s, b = http("POST", "/api/v1/auth/login", {"email": email, "password": password})
    if s != 200:
        raise SystemExit(f"login failed for {email}: {s} {b}")
    return b["access_token"]


def add_teammate(p, tag):
    """A second ACTIVE user on the same account, sharing the owner's password_hash (copied from the
    owner's row), so the ownership refusal is measured against a real non-owner teammate."""
    owner_hash = scalar(f"SELECT password_hash FROM users WHERE id = '{p['user_id']}'")
    email = f"probe-erasure-teammate-{tag}@example.invalid"
    sql(
        f"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, "
        f"perm_is_super_admin, permissions, password_undelivered, created_at, updated_at) "
        f"VALUES (gen_random_uuid(), '{p['aid']}', '{email}', '{owner_hash}', 'Erasure Teammate', "
        f"'user', true, false, '[]'::jsonb, false, NOW(), NOW());"
    )
    return email


def erase_tenant(aid):
    """Purge a probe root directly with the SQL writer (cleanup + the second probe's close-out)."""
    sql(f"SELECT erase_tenant('{aid}', '00000000-0000-0000-0000-000000000000', 'probe-erasure@example.invalid')")


def main():
    tag = time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(2)
    census_before = int(scalar("SELECT COUNT(*) FROM accounts") or 0)
    print(f"accounts census before: {census_before}")

    probe = register_probe(tag)
    print(f"probe account {probe['aid']} ({probe['email']})")
    api_key = mint_valid_api_key(probe)
    seed_rows(probe)
    before = snapshot(probe)
    invoice_id = scalar(f"SELECT id FROM invoices WHERE aid = '{probe['aid']}' LIMIT 1")
    print("before:", before)

    # ── 1. a VALID issued API key, both doors ───────────────────────────────────────────────────
    s, b = http("POST", "/api/v1/auth/close-account",
                {"password": probe["password"], "confirm": "CLOSE"}, api_key=api_key)
    check("1 close-account with a valid API key -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"}, api_key=api_key)
    check("2 erase-data with a valid API key -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")
    check("2b refused by key: rows untouched", snapshot(probe) == before, str(snapshot(probe)))

    # ── 3. no credential ────────────────────────────────────────────────────────────────────────
    s, _ = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"})
    check("3 no credential on erase -> 401", s == 401, f"HTTP {s}")

    # ── 4. wrong password / 5. wrong word ───────────────────────────────────────────────────────
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": "definitely-not-the-password", "confirm": "DELETE"},
                token=probe["token"])
    check("4 wrong password on erase -> 401", s == 401, f"HTTP {s} {str(b)[:120]}")
    check("4b ...and every probe row still exists", snapshot(probe) == before, str(snapshot(probe)))
    check("4c ...and no erasure audit row was written",
          scalar(f"SELECT COUNT(*) FROM account_erasures WHERE erased_tenant_id = '{probe['aid']}'") == "0")

    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "delete everything"},
                token=probe["token"])
    check("5 wrong typed word on erase -> 400", s == 400, f"HTTP {s} {str(b)[:120]}")
    check("5b ...and every probe row still exists", snapshot(probe) == before, str(snapshot(probe)))

    # ── 6. the real thing ───────────────────────────────────────────────────────────────────────
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"}, token=probe["token"])
    check("6a erase with session + password + DELETE -> 200", s == 200, f"HTTP {s} {str(b)[:200]}")
    bd = b if isinstance(b, dict) else {}
    check("6b reports account_erased", bd.get("status") == "account_erased", str(bd.get("status")))
    check("6c account row is gone",
          scalar(f"SELECT COUNT(*) FROM accounts WHERE id = '{probe['aid']}'") == "0")
    check("6d users/keys/workflows/plans are gone",
          all(snapshot(probe)[k] == 0 for k in ("users", "api_keys", "workflows", "account_plans")),
          str(snapshot(probe)))
    check("6e invoice row KEPT with aid NULL",
          scalar(f"SELECT COUNT(*) FROM invoices WHERE id = '{invoice_id}'") == "1"
          and scalar(f"SELECT COUNT(*) FROM invoices WHERE id = '{invoice_id}' AND aid IS NULL") == "1")
    kept = int(scalar(
        f"SELECT COUNT(*) FROM erasure_billing_retention WHERE erased_tenant_id = '{probe['aid']}'") or 0)
    check("6f billing documents kept", kept >= 1, f"kept {kept}")
    audit = sql(f"SELECT actor_email, action FROM account_erasures "
                f"WHERE erased_tenant_id = '{probe['aid']}'")
    check("6g exactly ONE erasure audit row", len(audit) == 1, f"{len(audit)} row(s)")
    if len(audit) == 1:
        check("6h audit names the actor and the action",
              audit[0][0] == probe["email"] and audit[0][1] == "erase_data", str(audit[0]))
    s, _ = http("GET", "/api/v1/auth/me", token=probe["token"])
    check("6i the erased session is dead (old token -> 401)", s == 401, f"HTTP {s}")
    s, _ = http("GET", "/api/v1/auth/me", api_key=api_key)
    check("6j the erased key is dead (old key -> 401)", s == 401, f"HTTP {s}")

    # ── 7. a teammate (second active user, non-owner) is refused by the ownership check ─────────
    p2 = register_probe(tag + "-teammate")
    mint_valid_api_key(p2)  # keep the script's own key usable; the teammate tests ownership, not the key
    teammate_email = add_teammate(p2, tag)
    teammate_token = login(teammate_email, p2["password"])
    b2 = snapshot(p2)
    s, b = http("POST", "/api/v1/auth/erase-data",
                {"password": p2["password"], "confirm": "DELETE"}, token=teammate_token)
    check("7 teammate (non-owner, 2nd user) on erase -> 403", s == 403, f"HTTP {s} {str(b)[:120]}")
    check("7b ...and the account's rows still exist", snapshot(p2) == b2, str(snapshot(p2)))

    # ── 8. cleanup: erase the second probe (and any residue) with the SQL writer ────────────────
    erase_tenant(p2["aid"])
    # The probes' own audit/retention rows are probe residue too (they name the probe accounts),
    # so they are purged along with any surviving account row.
    sql(f"DELETE FROM erasure_billing_retention "
        f"WHERE erased_tenant_id IN ('{probe['aid']}','{p2['aid']}');"
        f"DELETE FROM account_erasures "
        f"WHERE erased_tenant_id IN ('{probe['aid']}','{p2['aid']}');")
    residue = int(scalar(
        f"SELECT COUNT(*) FROM accounts WHERE id IN ('{probe['aid']}','{p2['aid']}')") or 0) + int(
        scalar(f"SELECT COUNT(*) FROM account_erasures "
               f"WHERE erased_tenant_id IN ('{probe['aid']}','{p2['aid']}')") or 0) + int(
        scalar(f"SELECT COUNT(*) FROM erasure_billing_retention "
               f"WHERE erased_tenant_id IN ('{probe['aid']}','{p2['aid']}')") or 0)
    check("8 probe residue cleaned", residue == 0, f"{residue} row(s) left")
    census_after = int(scalar("SELECT COUNT(*) FROM accounts") or 0)
    print(f"accounts census after: {census_after}")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
