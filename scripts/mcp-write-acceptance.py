#!/usr/bin/env python3
"""WorkflowSwift MCP WRITE door — live acceptance (kanban t_bb4d4d33).

Proves, against the RUNNING container, that the door went 10 -> 25 tools: five entities
(workflows, templates, dashboards, clients, tags) each gained create/update/archive, all
`ToolSpec::write` with `min_role Workspace`, soft-archived, tenant-scoped inside the SQL, and
audited with the key's label (never the key's bytes).

Checks, in order:

  1. tools/list for a WORKSPACE key is exactly 25 tools; all 15 write tools present with
     readOnlyHint=false; the 10 reads still readOnlyHint=true.
  2. tools/list for a READ-ONLY key (permissions=["read"]) is exactly the 10 reads — no write
     tool is offered — and a direct write call answers `forbidden` (isError), never a success.
  3. Per entity, with the workspace key: create -> read back -> update -> archive; the row is
     is_active=false in the DB afterwards, and the read tool no longer returns it (or the DB
     confirms it for entities without a read tool).
  4. A foreign id (a real row of a SECOND probe tenant) answers `not_found` for every entity's
     archive — never `forbidden`, never a foreign row.
  5. Platform-admin refusal: PUT /api/v1/admin/accounts/{id}/active with the API key answers 403.
  6. Audit: one JSON line per write carrying the key's label in `subject` and the entity id in
     `args` (for update/archive), with ZERO full key material anywhere in the log.
  7. The REAL CEO key (vault `workflowswift-mcp-giraudy`) sees the 25 tools and performs one
     create -> archive cycle on a row named `probe_t_bb4d4d33`, then the row is removed.
  8. Teardown: probe accounts (probe_harness='t_bb4d4d33') and every probe-named row are gone;
     residue reported = 0.

Probe workspaces are minted DIRECTLY in the database with `probe_harness='t_bb4d4d33'` (the same
convention as scripts/personal-api-keys-acceptance.py) — no mail, no signup, no spent credits. A
raw key is `workflowswift_<16 hex>`; key_hash is sha256_hex(raw); prefix is the first 8 hex.

Never prints a full key.

Usage: python3 scripts/mcp-write-acceptance.py [base_url]
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
PROBE = "t_bb4d4d33"
MARKER = "probe_t_bb4d4d33"
UA = "workflowswift-mcp-write-acceptance/1.0"
AUDIT = "/opt/swift/workflowswift/data/mcp-audit.log"

FAILED: list[str] = []

READS = [
    "workflowswift_get_run",
    "workflowswift_get_workflow",
    "workflowswift_list_clients",
    "workflowswift_list_dashboards",
    "workflowswift_list_integrations",
    "workflowswift_list_invoices",
    "workflowswift_list_runs",
    "workflowswift_list_templates",
    "workflowswift_list_triggers",
    "workflowswift_list_workflows",
]
WRITES = [
    "workflowswift_archive_client",
    "workflowswift_archive_dashboard",
    "workflowswift_archive_tag",
    "workflowswift_archive_template",
    "workflowswift_archive_workflow",
    "workflowswift_create_client",
    "workflowswift_create_dashboard",
    "workflowswift_create_tag",
    "workflowswift_create_template",
    "workflowswift_create_workflow",
    "workflowswift_update_client",
    "workflowswift_update_dashboard",
    "workflowswift_update_tag",
    "workflowswift_update_template",
    "workflowswift_update_workflow",
]

# One entity: its table, its three tools, and the fields its create accepts.
ENTITIES = [
    {
        "slug": "workflow",
        "table": "workflows",
        "create": "workflowswift_create_workflow",
        "update": "workflowswift_update_workflow",
        "archive": "workflowswift_archive_workflow",
        "create_args": {"description": "probe write acceptance"},
        "update_args": {"description": "probe write acceptance (updated)"},
        "read": "workflowswift_get_workflow",
    },
    {
        "slug": "template",
        "table": "workflow_templates",
        "create": "workflowswift_create_template",
        "update": "workflowswift_update_template",
        "archive": "workflowswift_archive_template",
        "create_args": {"category": "probe", "description": "probe write acceptance"},
        "update_args": {"description": "probe write acceptance (updated)"},
        "read": "workflowswift_list_templates",
    },
    {
        "slug": "dashboard",
        "table": "dashboards",
        "create": "workflowswift_create_dashboard",
        "update": "workflowswift_update_dashboard",
        "archive": "workflowswift_archive_dashboard",
        "create_args": {"description": "probe write acceptance"},
        "update_args": {"description": "probe write acceptance (updated)"},
        "read": "workflowswift_list_dashboards",
    },
    {
        "slug": "client",
        "table": "clients",
        "create": "workflowswift_create_client",
        "update": "workflowswift_update_client",
        "archive": "workflowswift_archive_client",
        "create_args": {"email": f"probe@{MARKER}.invalid"},
        "update_args": {"notes": "probe write acceptance (updated)"},
        "read": "workflowswift_list_clients",
    },
    {
        "slug": "tag",
        "table": "tags",
        "create": "workflowswift_create_tag",
        "update": "workflowswift_update_tag",
        "archive": "workflowswift_archive_tag",
        "create_args": {"color": "#123456"},
        "update_args": {"color": "#abcdef"},
        "read": None,  # no tag read tool; read back via the DB
    },
]


def check(name: str, ok: bool, detail: str = "") -> None:
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail and not ok else ""))
    if not ok:
        FAILED.append(name)


def pgpassword() -> str:
    r = subprocess.run(
        ["docker", "inspect", CONTAINER, "--format",
         "{{range .Config.Env}}{{println .}}{{end}}"],
        capture_output=True, text=True, timeout=30,
    )
    if r.returncode != 0:
        raise SystemExit(f"docker inspect failed: {r.stderr.strip()[:200]}")
    for line in r.stdout.splitlines():
        if line.startswith("DATABASE_URL=postgres://swift:"):
            return line[len("DATABASE_URL=postgres://swift:"):].split("@")[0]
    raise SystemExit("no DATABASE_URL in container env")


PGPASSWORD = pgpassword()


def sql(sql_text: str) -> str:
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


def rpc(token: str, method: str, params=None, ident=1):
    payload = {"jsonrpc": "2.0", "id": ident, "method": method}
    if params is not None:
        payload["params"] = params
    return http("POST", "/mcp", payload, api_key=token)


def call(token: str, tool: str, args: dict | None = None, ident=100):
    """One tools/call. Returns (is_error: bool, payload: dict)."""
    status, body = rpc(token, "tools/call", {"name": tool, "arguments": args or {}}, ident)
    result = body.get("result") or {}
    payload = result.get("structuredContent")
    if payload is None:
        text = (result.get("content") or [{}])[0].get("text")
        try:
            payload = json.loads(text) if text else None
        except json.JSONDecodeError:
            payload = None
    payload = payload if isinstance(payload, dict) else {}
    return bool(result.get("isError")), payload


def tool_names(token: str) -> list[str]:
    _, body = rpc(token, "tools/list", ident=1)
    tools = (body.get("result") or {}).get("tools") or []
    return sorted(t.get("name") for t in tools)


def make_probe_account(tag: str, key_label: str, key_permissions: str) -> dict:
    """One probe account + user + api key, written directly in the DB (probe_harness marked)."""
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
    email = f"probe-{tag}-{key_label}@example.invalid"
    # role 'user' (not 'admin'): `idx_unique_admin` allows exactly ONE user with role='admin'
    # across the whole table, and the MCP door's resolver never reads users.role — the key's own
    # `permissions` decide read vs write.
    sql(
        f"INSERT INTO accounts (id, name, account_slug, plan_id, probe_harness, is_active) "
        f"VALUES ('{aid}', 'Probe {tag} {key_label}', 'probe-{tag}-{key_label}', '{plan}', '{PROBE}', true);"
        f"INSERT INTO users (id, aid, email, password_hash, name, role, is_active) "
        f"VALUES ('{uid}', '{aid}', '{email}', 'x', 'Probe {tag} {key_label}', 'user', true);"
        f"INSERT INTO api_keys (id, aid, user_id, name, key_hash, prefix, permissions, is_active) "
        f"VALUES ('{kid}', '{aid}', '{uid}', '{key_label}', '{key_hash}', '{prefix}', '{key_permissions}', true);"
    )
    return {"aid": aid, "uid": uid, "key_id": kid, "raw_key": raw_key,
            "prefix": prefix, "label": key_label}


def get_ceo_key() -> str:
    env = {}
    with open("/opt/swift/secure/cred.env", "r", encoding="utf-8") as fh:
        for line in fh:
            if "=" in line:
                k, v = line.strip().split("=", 1)
                env[k] = v
    token = env.get("CRED_TOKEN", "")
    if not token:
        raise SystemExit("no CRED_TOKEN in /opt/swift/secure/cred.env")
    req = urllib.request.Request(
        "http://127.0.0.1:8100/vault/workflowswift-mcp-giraudy",
        headers={"Authorization": "Bearer " + token},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        body = json.loads(r.read().decode())
    key = body.get("password") or ""
    if not key.startswith("workflowswift_"):
        raise SystemExit("vault record has no workflowswift_ key in `password`")
    return key


def audit_lines() -> list[dict]:
    try:
        with open(AUDIT, "r", encoding="utf-8") as fh:
            return [json.loads(line) for line in fh if line.strip()]
    except (FileNotFoundError, json.JSONDecodeError):
        return []


def main() -> int:
    tag = time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(2)

    # Two probe accounts: the owner (write + read keys) and a second tenant (write key only).
    owner = make_probe_account(tag, "owner-write", "[]")
    owner_read = make_probe_account(tag, "owner-read", '["read"]')
    second = make_probe_account(tag, "second", "[]")
    owner_key = owner["raw_key"]
    read_key = owner_read["raw_key"]
    second_key = second["raw_key"]
    owner_aid = owner["aid"]

    print(f"probe owner  aid={owner_aid} prefix={owner['prefix']}")
    print(f"probe owner  read key prefix={owner_read['prefix']}")
    print(f"probe second aid={second['aid']} prefix={second['prefix']}")

    before_audit = len(audit_lines())

    # ── 1. the tool surface: 10 -> 25 ───────────────────────────────────────────────────────────
    names = tool_names(owner_key)
    check("tools/list = exactly 25 tools", len(names) == 25, f"got {len(names)}")
    check("all 15 write tools present", all(w in names for w in WRITES),
          f"missing {[w for w in WRITES if w not in names]}")
    check("all 10 reads still present", all(r in names for r in READS))
    _, body = rpc(owner_key, "tools/list", ident=1)
    tools = (body.get("result") or {}).get("tools") or []
    by_name = {t["name"]: t for t in tools}
    write_hints = [by_name[w].get("annotations", {}).get("readOnlyHint") for w in WRITES
                   if w in by_name]
    read_hints = [by_name[r].get("annotations", {}).get("readOnlyHint") for r in READS
                  if r in by_name]
    check("every write tool readOnlyHint=false", len(write_hints) == 15 and not any(write_hints),
          f"{write_hints}")
    check("every read tool readOnlyHint=true", len(read_hints) == 10 and all(read_hints))

    # ── 2. read-only key: no write tools, direct call refused ──────────────────────────────────
    ro_names = tool_names(read_key)
    check("read-only key sees exactly the 10 reads", ro_names == sorted(READS),
          f"got {len(ro_names)} tools")
    is_err, payload = call(read_key, "workflowswift_create_workflow", {"name": "nope"})
    check("read-only key direct write call refused (forbidden)",
          is_err and payload.get("error") == "forbidden", f"{payload}")

    # ── 3. per entity: create -> read back -> update -> archive ────────────────────────────────
    owner_ids: dict[str, str] = {}
    foreign_ids: dict[str, str] = {}

    for e in ENTITIES:
        name = f"{MARKER}-{e['slug']}-{secrets.token_hex(3)}"

        # the SECOND tenant also owns one real row per entity, for the foreign-id check
        is_err, fpayload = call(second_key, e["create"], {"name": f"{MARKER}-foreign-{e['slug']}-{secrets.token_hex(3)}", **e["create_args"]})
        if not is_err and fpayload.get("id"):
            foreign_ids[e["slug"]] = fpayload["id"]

        # create
        is_err, payload = call(owner_key, e["create"], {"name": name, **e["create_args"]})
        check(f"{e['slug']} create -> id", not is_err and bool(payload.get("id")), f"{payload}")
        row_id = payload.get("id")
        if not row_id:
            continue
        owner_ids[e["slug"]] = row_id

        # read back (the entity's read tool where one exists, else the DB)
        if e["read"]:
            is_err, rp = call(owner_key, e["read"], {"id": row_id})
            seen = False
            if e["read"] == "workflowswift_get_workflow":
                seen = bool((rp or {}).get("workflow", {}).get("id") == row_id)
            else:
                items = (rp or {}).get("items") or []
                seen = any(i.get("id") == row_id for i in items)
            check(f"{e['slug']} read back", not is_err and seen, f"{rp}")
        else:
            db_seen = scalar(f"SELECT count(*) FROM {e['table']} WHERE id = '{row_id}' AND aid = '{owner_aid}'")
            check(f"{e['slug']} read back (db)", db_seen == "1", f"count={db_seen}")

        # update
        is_err, payload = call(owner_key, e["update"], {"id": row_id, **e["update_args"]})
        check(f"{e['slug']} update -> same id", not is_err and payload.get("id") == row_id,
              f"{payload}")

        # archive
        is_err, payload = call(owner_key, e["archive"], {"id": row_id})
        check(f"{e['slug']} archive -> same id", not is_err and payload.get("id") == row_id,
              f"{payload}")

        # archived in the DB
        is_active = scalar(f"SELECT coalesce(is_active, false) FROM {e['table']} WHERE id = '{row_id}'")
        check(f"{e['slug']} archived in DB (is_active=false)", is_active == "f", f"is_active={is_active}")

        # the read tool no longer returns it (where the read filters is_active)
        if e["read"] in ("workflowswift_list_templates", "workflowswift_list_clients", "workflowswift_list_workflows"):
            # templates always filter active; clients/workflows accept active_only
            args = {"active_only": True} if e["read"] != "workflowswift_list_templates" else {}
            is_err, rp = call(owner_key, e["read"], args)
            items = (rp or {}).get("items") or []
            check(f"{e['slug']} no longer listed active", all(i.get("id") != row_id for i in items),
                  f"{rp}")

    # ── 4. foreign id -> not_found, for every entity ───────────────────────────────────────────
    for e in ENTITIES:
        fid = foreign_ids.get(e["slug"])
        if not fid:
            check(f"{e['slug']} foreign id not_found", False, "no foreign row minted")
            continue
        is_err, payload = call(owner_key, e["archive"], {"id": fid})
        check(f"{e['slug']} foreign id -> not_found",
              is_err and payload.get("error") == "not_found", f"{payload}")

    # ── 5. platform-admin refusal ──────────────────────────────────────────────────────────────
    s, b = http("PUT", f"/api/v1/admin/accounts/{second['aid']}/active",
                {"is_active": True}, api_key=owner_key)
    check("PUT /api/v1/admin/accounts/{id}/active -> 403 for the API key", s == 403, f"HTTP {s}")

    # ── 6. audit ───────────────────────────────────────────────────────────────────────────────
    lines = audit_lines()
    new_lines = lines[before_audit:]
    write_lines = [l for l in new_lines if l.get("tool") in WRITES]
    # 5 second creates + 5 owner creates + 5 owner updates + 5 owner archives + 5 foreign archives.
    check("one audit line per write call", len(write_lines) >= 25,
          f"write lines={len(write_lines)}")
    ok_writes = [l for l in write_lines if l.get("outcome") == "ok"]
    known_labels = ("owner-write", "second")
    labelled = all(
        (l.get("subject") or "").startswith("workflowswift:")
        and any(lab in (l.get("subject") or "") for lab in known_labels)
        for l in ok_writes
    )
    check("every write audited with the calling key's label (never the account id alone)",
          bool(ok_writes) and labelled,
          f"subjects={sorted({l.get('subject') for l in ok_writes})}")
    owner_ok = [l for l in ok_writes if "owner-write" in (l.get("subject") or "")]
    check("the owner's own writes carry the owner key label", len(owner_ok) >= 15,
          f"owner lines={len(owner_ok)}")
    # the owner's own update/archive (success) lines carry one of the five entity ids in args
    owner_id_lines = [l for l in owner_ok
                      if l.get("tool", "").startswith(("workflowswift_update_", "workflowswift_archive_"))]
    ids_in_args = bool(owner_id_lines) and all(
        any(rid in (l.get("args") or "") for rid in owner_ids.values())
        for l in owner_id_lines
    )
    check("update/archive audit args carry the entity id", ids_in_args,
          f"id lines={len(owner_id_lines)}")
    blob = json.dumps(lines)
    secrets_clean = not any(
        k in blob or k[:16] in blob for k in (owner_key, second_key, read_key)
    )
    check("ZERO full key material in the audit", secrets_clean)

    # ── 7. the REAL CEO key sees the tools and does one create -> archive cycle ────────────────
    try:
        ceo_key = get_ceo_key()
        ceo_names = tool_names(ceo_key)
        check("CEO key sees all 25 tools", len(ceo_names) == 25, f"got {len(ceo_names)}")
        ceo_aid = scalar(f"SELECT aid FROM api_keys WHERE key_hash = '{sha256_hex(ceo_key)}'")
        ceo_name = f"{MARKER}-ceo-{secrets.token_hex(3)}"
        is_err, payload = call(ceo_key, "workflowswift_create_workflow", {"name": ceo_name})
        ceo_ok = not is_err and bool(payload.get("id"))
        check("CEO create -> id", ceo_ok, f"{payload}")
        if ceo_ok:
            ceo_id = payload["id"]
            is_err, payload = call(ceo_key, "workflowswift_archive_workflow", {"id": ceo_id})
            check("CEO archive -> same id", not is_err and payload.get("id") == ceo_id, f"{payload}")
            # remove the CEO's probe row (hard, only this marker-named row)
            sql(f"DELETE FROM workflows WHERE id = '{ceo_id}' AND aid = '{ceo_aid}' AND name = '{ceo_name}'")
            left = scalar(f"SELECT count(*) FROM workflows WHERE id = '{ceo_id}'")
            check("CEO probe row removed", left == "0", f"left={left}")
        else:
            check("CEO archive -> same id", False, "no create id")
    except Exception as exc:  # the vault is required; a missing vault fails the CEO leg loudly
        check("CEO key create/archive cycle", False, f"vault error: {exc}")

    # ── 8. teardown + residue ──────────────────────────────────────────────────────────────────
    sql(
        f"DELETE FROM api_key_reveals WHERE aid IN ('{owner_aid}','{owner_read['aid']}','{second['aid']}');"
        f"DELETE FROM api_key_events WHERE owner_id IN ('{owner_aid}','{owner_read['aid']}','{second['aid']}');"
        f"DELETE FROM accounts WHERE id IN ('{owner_aid}','{owner_read['aid']}','{second['aid']}');"
    )
    residue_accounts = scalar(f"SELECT count(*) FROM accounts WHERE probe_harness = '{PROBE}'")
    residue_parts = [f"accounts={residue_accounts}"]
    residue_total = int(residue_accounts or 0)
    for e in ENTITIES:
        n = scalar(f"SELECT count(*) FROM {e['table']} WHERE name LIKE '{MARKER}%'")
        residue_total += int(n or 0)
        residue_parts.append(f"{e['table']}={n}")
    check("probe residue = 0", residue_total == 0, " ".join(residue_parts))

    print()
    print(f"tool count: 10 -> {len(names)}")
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print(f"ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
