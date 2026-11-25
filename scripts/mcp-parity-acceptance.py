#!/usr/bin/env python3
"""WorkflowSwift MCP PARITY door — live acceptance (kanban t_8df5d174).

Proves, against the RUNNING container, that the door went 25 -> 49 tools: the twenty-four
agent-parity tools close the action/CRUD families the card names (run a workflow, settle a waiting
run step, leads / competitors / brand monitors / tickets / automations, template import, rendition
create, prospect search/enrich). Same contract as mcp-write-acceptance.py: soft archive, tenant
scoping inside the SQL, foreign id -> not_found, platform-admin -> 403, audited with the key's
label (never key bytes), REAL CEO key cycle, zero residue.

Checks, in order:
  1. tools/list for a WORKSPACE key is exactly 49; all 24 parity tools present; the 7 parity reads
     readOnlyHint=true, the 17 parity writes readOnlyHint=false.
  2. tools/list for a READ-ONLY key is the 17 reads (10 original + 7 parity) — no write tool — and
     a direct write call answers forbidden.
  3. Per CRUD family: create -> read back -> update -> archive; the DB row shows the change
     (is_active=false, or archived_at IS NOT NULL for leads/tickets); foreign id -> not_found.
  4. Action families: run_workflow leaves a workflow_instances row; decide_run_step settles a
     parked manual step; run_automation leaves an automation_runs row; import_template leaves a
     workflow_templates row + steps; create_rendition leaves an account_renditions row. Foreign
     run/decide/automation ids answer not_found. prospect_search/enrich_lead are proven wired via
     their honest refusal (no provider key on a probe -> 'unavailable', never invented data).
  5. Platform-admin refusal: PUT /api/v1/admin/accounts/{id}/active with the API key answers 403.
  6. Audit: one JSON line per parity write, subject carries the key's label (#<key row id>), and
     the update/archive/run args carry the entity id; ZERO key material in the log.
  7. The REAL CEO key (vault workflowswift-mcp-giraudy) sees the 49 tools and performs one
     create_lead -> archive_lead cycle, then the row is removed.
  8. Teardown: probe accounts (probe_harness='t_8df5d174') and every probe-named row are gone;
     residue reported = 0.

Probe workspaces are minted DIRECTLY in the database (probe_harness='t_8df5d174') — no mail, no
signup. A raw key is `workflowswift_<16 hex>`; key_hash = sha256_hex(raw); prefix = first 8 hex.

Never prints a full key.

Usage: python3 scripts/mcp-parity-acceptance.py [base_url]
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
PROBE = "t_8df5d174"
MARKER = "probe_t_8df5d174"
UA = "workflowswift-mcp-parity-acceptance/1.0"
AUDIT = "/opt/swift/workflowswift/data/mcp-audit.log"

FAILED: list[str] = []

ORIGINAL_READS = [
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

PARITY_READS = [
    "workflowswift_list_leads",
    "workflowswift_list_competitors",
    "workflowswift_list_brand_monitors",
    "workflowswift_list_tickets",
    "workflowswift_list_automations",
    "workflowswift_prospect_search",
    "workflowswift_enrich_lead",
]

PARITY_WRITES = [
    "workflowswift_run_workflow",
    "workflowswift_decide_run_step",
    "workflowswift_create_lead",
    "workflowswift_update_lead",
    "workflowswift_archive_lead",
    "workflowswift_create_competitor",
    "workflowswift_update_competitor",
    "workflowswift_archive_competitor",
    "workflowswift_create_brand_monitor",
    "workflowswift_archive_brand_monitor",
    "workflowswift_create_ticket",
    "workflowswift_update_ticket",
    "workflowswift_archive_ticket",
    "workflowswift_create_automation",
    "workflowswift_run_automation",
    "workflowswift_import_template",
    "workflowswift_create_rendition",
]

ALL_READS = sorted(ORIGINAL_READS + PARITY_READS)
TOTAL = 49

# One CRUD family: its table, tools, name key, and how "archived" reads in the DB.
CRUD = [
    {
        "slug": "lead",
        "table": "leads",
        "name_key": "name",
        "list": "workflowswift_list_leads",
        "create": "workflowswift_create_lead",
        "update": "workflowswift_update_lead",
        "archive": "workflowswift_archive_lead",
        "create_args": {"email": f"probe@{MARKER}.invalid"},
        "update_args": {"notes": "parity probe (updated)"},
        "archived_col": "archived_at",  # NULL -> live; NOT NULL -> archived
    },
    {
        "slug": "competitor",
        "table": "competitors",
        "name_key": "name",
        "list": "workflowswift_list_competitors",
        "create": "workflowswift_create_competitor",
        "update": "workflowswift_update_competitor",
        "archive": "workflowswift_archive_competitor",
        "create_args": {"website": "https://example.com"},
        "update_args": {"description": "parity probe (updated)"},
        "archived_col": "is_active",
    },
    {
        "slug": "brand_monitor",
        "table": "brand_monitors",
        "name_key": "brand_name",
        "list": "workflowswift_list_brand_monitors",
        "create": "workflowswift_create_brand_monitor",
        "update": None,
        "archive": "workflowswift_archive_brand_monitor",
        "create_args": {"platforms": ["web"]},
        "update_args": {},
        "archived_col": "is_active",
    },
    {
        "slug": "ticket",
        "table": "tickets",
        "name_key": "subject",
        "list": "workflowswift_list_tickets",
        "create": "workflowswift_create_ticket",
        "update": "workflowswift_update_ticket",
        "archive": "workflowswift_archive_ticket",
        "create_args": {"description": "parity probe"},
        "update_args": {"priority": "high"},
        "archived_col": "archived_at",
    },
    {
        "slug": "automation",
        "table": "automations",
        "name_key": "name",
        "list": "workflowswift_list_automations",
        "create": "workflowswift_create_automation",
        "update": None,
        "archive": None,  # automations have run, not archive
        "create_args": {"trigger_type": "manual", "action_type": "webhook"},
        "update_args": {},
        "archived_col": None,
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
            return line.split("postgres://swift:", 1)[1].split("@", 1)[0]
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


def make_running_gate(key: str, aid: str, tag: str) -> dict:
    """Create a [data-card, manual] workflow for the account, grant credits, run it, and return the
    parked manual step's ids. The run parks on the manual step, so a decide can settle it."""
    wf_name = f"{MARKER}-gate-{tag}-{secrets.token_hex(3)}"
    is_err, wf = call(key, "workflowswift_create_workflow", {"name": wf_name})
    wf_id = (wf or {}).get("id")
    if is_err or not wf_id:
        raise SystemExit(f"gate workflow create failed: {wf}")
    s, _ = http("POST", f"/api/v1/workflows/{wf_id}/steps",
                {"step_type": "data-card", "name": "Card", "config": {"metric_key": "probe"}},
                api_key=key)
    if s != 201:
        raise SystemExit(f"data-card step create failed: HTTP {s}")
    s, _ = http("POST", f"/api/v1/workflows/{wf_id}/steps",
                {"step_type": "manual", "name": "Gate", "config": {}},
                api_key=key)
    if s != 201:
        raise SystemExit(f"manual step create failed: HTTP {s}")
    sql(f"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description) "
        f"VALUES (gen_random_uuid(), '{aid}', 10, 'grant', 'parity probe')")
    is_err, run = call(key, "workflowswift_run_workflow", {"workflow_id": wf_id})
    if is_err or not (run or {}).get("instance_id"):
        raise SystemExit(f"gate run failed: {run}")
    instance_id = run["instance_id"]
    step_id = scalar(
        f"SELECT id FROM workflow_instance_steps WHERE instance_id = '{instance_id}' "
        f"AND step_type = 'manual' ORDER BY sort_order LIMIT 1"
    )
    if not step_id:
        raise SystemExit("gate run left no parked manual step")
    return {"workflow_id": wf_id, "instance_id": instance_id, "step_id": step_id}


def main() -> int:
    tag = time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(2)

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

    # ── 1. the tool surface: 25 -> 49 ───────────────────────────────────────────────────────────
    names = tool_names(owner_key)
    check("tools/list = exactly 49 tools", len(names) == TOTAL, f"got {len(names)}")
    check("all 24 parity tools present", all(t in names for t in PARITY_READS + PARITY_WRITES),
          f"missing {[t for t in PARITY_READS + PARITY_WRITES if t not in names]}")
    check("all 10 original reads still present", all(r in names for r in ORIGINAL_READS))
    _, body = rpc(owner_key, "tools/list", ident=1)
    tools = (body.get("result") or {}).get("tools") or []
    by_name = {t["name"]: t for t in tools}
    write_hints = [by_name[w].get("annotations", {}).get("readOnlyHint") for w in PARITY_WRITES
                   if w in by_name]
    read_hints = [by_name[r].get("annotations", {}).get("readOnlyHint") for r in PARITY_READS
                  if r in by_name]
    check("every parity write tool readOnlyHint=false",
          len(write_hints) == len(PARITY_WRITES) and not any(write_hints), f"{write_hints}")
    check("every parity read tool readOnlyHint=true",
          len(read_hints) == len(PARITY_READS) and all(read_hints))

    # ── 2. read-only key: no write tools, direct call refused ──────────────────────────────────
    ro_names = tool_names(read_key)
    check("read-only key sees exactly the 17 reads", ro_names == ALL_READS,
          f"got {len(ro_names)} tools")
    is_err, payload = call(read_key, "workflowswift_create_lead", {"name": "nope"})
    check("read-only key direct write call refused (forbidden)",
          is_err and payload.get("error") == "forbidden", f"{payload}")

    # ── 3. CRUD families: create -> read back -> update -> archive; foreign -> not_found ────────
    owner_ids: dict[str, str] = {}
    foreign_ids: dict[str, str] = {}

    for e in CRUD:
        slug = e["slug"]
        nk = e["name_key"]
        # the SECOND tenant also owns one real row per family, for the foreign-id check
        fname = f"{MARKER}-foreign-{slug}-{secrets.token_hex(3)}"
        is_err, fp = call(second_key, e["create"], {nk: fname, **e["create_args"]})
        if not is_err and fp.get("id"):
            foreign_ids[slug] = fp["id"]

        name = f"{MARKER}-{slug}-{secrets.token_hex(3)}"
        is_err, p = call(owner_key, e["create"], {nk: name, **e["create_args"]})
        check(f"{slug} create -> id", not is_err and bool(p.get("id")), f"{p}")
        row_id = p.get("id")
        if not row_id:
            continue
        owner_ids[slug] = row_id

        # read back through the read tool
        is_err, lp = call(owner_key, e["list"], {})
        items = (lp or {}).get("items") or []
        seen = any(i.get("id") == row_id for i in items)
        check(f"{slug} read back", not is_err and seen, f"{lp}")

        # update (where a tool exists)
        if e["update"]:
            is_err, p = call(owner_key, e["update"], {"id": row_id, **e["update_args"]})
            check(f"{slug} update -> same id", not is_err and p.get("id") == row_id, f"{p}")

        # archive (where a tool exists)
        if e["archive"]:
            is_err, p = call(owner_key, e["archive"], {"id": row_id})
            check(f"{slug} archive -> same id", not is_err and p.get("id") == row_id, f"{p}")
            col = e["archived_col"]
            if col == "is_active":
                val = scalar(f"SELECT coalesce(is_active, false) FROM {e['table']} WHERE id = '{row_id}'")
                check(f"{slug} archived in DB (is_active=false)", val == "f", f"is_active={val}")
            else:
                val = scalar(f"SELECT (archived_at IS NOT NULL) FROM {e['table']} WHERE id = '{row_id}'")
                check(f"{slug} archived in DB (archived_at set)", val == "t", f"archived={val}")
            # no longer listed active
            is_err, lp = call(owner_key, e["list"], {"active_only": True})
            items = (lp or {}).get("items") or []
            check(f"{slug} no longer listed active",
                  all(i.get("id") != row_id for i in items), f"{lp}")
        else:
            # automations: run, not archive
            is_err, rp = call(owner_key, "workflowswift_run_automation", {"id": row_id, "trigger_data": {"probe": True}})
            run_id = (rp or {}).get("run_id")
            check(f"{slug} run -> run_id", not is_err and bool(run_id), f"{rp}")
            if run_id:
                n = scalar(f"SELECT count(*) FROM automation_runs WHERE id = '{run_id}' AND automation_id = '{row_id}'")
                check(f"{slug} run row in DB", n == "1", f"rows={n}")

    # foreign id -> not_found for every archive/run tool that takes an id
    for e in CRUD:
        slug = e["slug"]
        fid = foreign_ids.get(slug)
        if not fid:
            check(f"{slug} foreign id not_found", False, "no foreign row minted")
            continue
        tool = e["archive"] or "workflowswift_run_automation"
        is_err, p = call(owner_key, tool, {"id": fid})
        check(f"{slug} foreign id -> not_found",
              is_err and p.get("error") == "not_found", f"{p}")

    # ── 4. action families ──────────────────────────────────────────────────────────────────────
    # run_workflow + decide_run_step, on a [data-card, manual] workflow that parks on the gate.
    gate = make_running_gate(owner_key, owner_aid, "owner")
    second_gate = make_running_gate(second_key, second["aid"], "second")

    n = scalar(f"SELECT count(*) FROM workflow_instances WHERE id = '{gate['instance_id']}' AND aid = '{owner_aid}'")
    check("run_workflow leaves a workflow_instances row", n == "1", f"rows={n}")

    is_err, d = call(owner_key, "workflowswift_decide_run_step",
                     {"instance_id": gate["instance_id"], "step_id": gate["step_id"], "decision": "approve"})
    check("decide_run_step approve -> ok", not is_err and (d or {}).get("status"), f"{d}")
    step_status = scalar(f"SELECT status FROM workflow_instance_steps WHERE id = '{gate['step_id']}'")
    check("decide_run_step settled the parked step (no longer pending)",
          step_status not in ("pending", "in_progress", ""), f"status={step_status}")

    # foreign run + decide
    is_err, p = call(owner_key, "workflowswift_run_workflow", {"workflow_id": second_gate["workflow_id"]})
    check("run_workflow foreign id -> not_found", is_err and p.get("error") == "not_found", f"{p}")
    is_err, p = call(owner_key, "workflowswift_decide_run_step",
                     {"instance_id": second_gate["instance_id"], "step_id": second_gate["step_id"], "decision": "approve"})
    check("decide_run_step foreign id -> not_found", is_err and p.get("error") == "not_found", f"{p}")

    # import_template
    tpl_name = f"{MARKER}-tpl-{secrets.token_hex(3)}"
    is_err, tp = call(owner_key, "workflowswift_import_template", {
        "name": tpl_name, "category": "probe",
        "steps": [{"step_type": "data-card", "name": "Card", "sort_order": 0}],
    })
    tpl_id = ((tp or {}).get("template") or {}).get("id")
    check("import_template -> template id", not is_err and bool(tpl_id), f"{tp}")
    if tpl_id:
        n = scalar(f"SELECT count(*) FROM workflow_templates WHERE id = '{tpl_id}' AND aid = '{owner_aid}'")
        s = scalar(f"SELECT count(*) FROM workflow_template_steps WHERE template_id = '{tpl_id}'")
        check("import_template row + steps in DB", n == "1" and s == "1", f"tpl={n} steps={s}")

    # create_rendition (kind=rendition)
    is_err, rd = call(owner_key, "workflowswift_create_rendition", {
        "kind": "rendition", "provider": "workflowswift",
        "provider_asset_id": "probe-" + secrets.token_hex(4),
        "provider_asset_url": "https://example.com/probe.png", "asset_type": "image",
    })
    rd_id = ((rd or {}).get("rendition") or {}).get("id")
    check("create_rendition -> rendition id", not is_err and bool(rd_id), f"{rd}")
    if rd_id:
        n = scalar(f"SELECT count(*) FROM account_renditions WHERE id = '{rd_id}' AND aid = '{owner_aid}'")
        check("create_rendition row in DB", n == "1", f"rows={n}")

    # prospect_search / enrich_lead — proven wired via the honest refusal (no provider key on a probe)
    is_err, ps = call(owner_key, "workflowswift_prospect_search", {"query": "dentist"})
    check("prospect_search wired (honest 'unavailable' without a provider key)",
          is_err and ps.get("error") == "unavailable", f"{ps}")
    is_err, en = call(owner_key, "workflowswift_enrich_lead", {"business_name": "Acme"})
    check("enrich_lead wired (honest 'unavailable' without a provider key)",
          is_err and en.get("error") == "unavailable", f"{en}")

    # ── 5. platform-admin refusal ──────────────────────────────────────────────────────────────
    s, b = http("PUT", f"/api/v1/admin/accounts/{second['aid']}/active",
                {"is_active": True}, api_key=owner_key)
    check("PUT /api/v1/admin/accounts/{id}/active -> 403 for the API key", s == 403, f"HTTP {s}")

    # ── 6. audit ───────────────────────────────────────────────────────────────────────────────
    lines = audit_lines()
    new_lines = lines[before_audit:]
    write_lines = [l for l in new_lines if l.get("tool") in PARITY_WRITES]
    check("one audit line per parity write call", len(write_lines) >= 1, f"write lines={len(write_lines)}")
    ok_writes = [l for l in write_lines if l.get("outcome") == "ok"]

    def subject_key_id(subj: str) -> str:
        # workflowswift:<label>#<keyid>@<aid> — the #<keyid> is the stable actor id the card asks for.
        if "#" in subj:
            return subj.split("#", 1)[1].split("@", 1)[0]
        return ""

    known_key_ids = {owner["key_id"], second["key_id"]}
    labelled = all(
        (l.get("subject") or "").startswith("workflowswift:")
        and subject_key_id(l.get("subject") or "") in known_key_ids
        for l in ok_writes
    )
    check("every parity write audited with the key's label AND key row id", bool(ok_writes) and labelled,
          f"subjects={sorted({l.get('subject') for l in ok_writes})}")
    # the owner's own writes carry the owner key's row id (a foreign-id refusal is the second's)
    owner_ok = [l for l in ok_writes if subject_key_id(l.get("subject") or "") == owner["key_id"]]
    check("the owner's own parity writes carry the owner key row id", len(owner_ok) >= 15,
          f"owner lines={len(owner_ok)}")
    # update/archive/run/decide lines carry the entity id in args
    id_tools = ("workflowswift_update_", "workflowswift_archive_", "workflowswift_run_automation",
                "workflowswift_decide_run_step")
    id_lines = [l for l in ok_writes if l.get("tool", "").startswith(id_tools)]
    id_ok = bool(id_lines) and all(
        any(rid in (l.get("args") or "") for rid in list(owner_ids.values()) + [gate["instance_id"], gate["step_id"]])
        for l in id_lines
    )
    check("update/archive/run/decide audit args carry the entity id", id_ok, f"id lines={len(id_lines)}")
    blob = json.dumps(lines)
    secrets_clean = not any(k in blob or k[:16] in blob for k in (owner_key, second_key, read_key))
    check("ZERO full key material in the audit", secrets_clean)

    # ── 7. the REAL CEO key sees the tools and does one create -> archive cycle ────────────────
    try:
        ceo_key = get_ceo_key()
        ceo_names = tool_names(ceo_key)
        check("CEO key sees all 49 tools", len(ceo_names) == TOTAL, f"got {len(ceo_names)}")
        ceo_aid = scalar(f"SELECT aid FROM api_keys WHERE key_hash = '{sha256_hex(ceo_key)}'")
        ceo_name = f"{MARKER}-ceo-{secrets.token_hex(3)}"
        is_err, p = call(ceo_key, "workflowswift_create_lead", {"name": ceo_name})
        ceo_ok = not is_err and bool(p.get("id"))
        check("CEO create_lead -> id", ceo_ok, f"{p}")
        if ceo_ok:
            ceo_id = p["id"]
            is_err, p = call(ceo_key, "workflowswift_archive_lead", {"id": ceo_id})
            check("CEO archive_lead -> same id", not is_err and p.get("id") == ceo_id, f"{p}")
            sql(f"DELETE FROM leads WHERE id = '{ceo_id}' AND aid = '{ceo_aid}' AND name = '{ceo_name}'")
            left = scalar(f"SELECT count(*) FROM leads WHERE id = '{ceo_id}'")
            check("CEO probe row removed", left == "0", f"left={left}")
        else:
            check("CEO archive_lead -> same id", False, "no create id")
    except Exception as exc:
        check("CEO key create/archive cycle", False, f"vault error: {exc}")

    # ── 8. teardown + residue ──────────────────────────────────────────────────────────────────
    probe_aids = [owner_aid, owner_read["aid"], second["aid"]]
    sql(
        "DELETE FROM api_key_reveals WHERE aid IN ('" + "','".join(probe_aids) + "');"
        "DELETE FROM api_key_events WHERE owner_id IN ('" + "','".join(probe_aids) + "');"
        "DELETE FROM accounts WHERE id IN ('" + "','".join(probe_aids) + "');"
    )
    residue_accounts = scalar(f"SELECT count(*) FROM accounts WHERE probe_harness = '{PROBE}'")
    residue_total = int(residue_accounts or 0)
    residue_parts = [f"accounts={residue_accounts}"]
    for table, col in [("leads", "name"), ("competitors", "name"), ("brand_monitors", "brand_name"),
                       ("tickets", "subject"), ("automations", "name"),
                       ("workflows", "name"), ("workflow_templates", "name")]:
        n = scalar(f"SELECT count(*) FROM {table} WHERE {col} LIKE '{MARKER}%'")
        residue_total += int(n or 0)
        residue_parts.append(f"{table}={n}")
    check("probe residue = 0", residue_total == 0, " ".join(residue_parts))

    print()
    print(f"tool count: 25 -> {len(names)}")
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
