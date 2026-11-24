#!/usr/bin/env python3
"""WorkflowSwift MCP door — the app-side acceptance probe (kanban t_6364d995).

`door-contract.py` (in the `mcp-door` crate) proves the TRANSPORT and PROTOCOL shape, which is the
same in every product. This script proves the three things only THIS app can be asked about:

  1  both workspaces authenticate, and each sees ONLY its own rows — a second workspace's counts
     differ and its ids are disjoint from the owner's (the token is the tenancy boundary);
  2  a run id that belongs to the owner answers `not_found` for the second workspace while the
     owner still reads it — the 404-not-403 rule, both directions in one run;
  3  the audit file is `0600`, gains exactly ONE line per tool call, and contains ZERO token
     strings (the bearer must never be written, not even truncated).

It also checks the tool surface (ten reads, all annotated read-only), that no payload projects a
secret, and that customer text is returned as a capped excerpt.

Usage:
  python3 scripts/mcp-door-acceptance.py \
      --url https://app.workflowswift.com/mcp \
      --owner-token workflowswift_... --second-token workflowswift_... \
      --audit /opt/swift/workflowswift/data/mcp-audit.log

Exit 0 = every check passed. 1 = at least one failed (printed).
"""
from __future__ import annotations

import argparse
import json
import os
import stat
import sys
import urllib.error
import urllib.request

UA = "workflowswift-mcp-acceptance/1.0"
TOOLS = [
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

# Field names that must never appear in a payload. Customer text can contain anything, so the
# check is on JSON KEYS, not values (a client named "api_key" is not a leak of a credential).
FORBIDDEN_KEYS = {
    "api_key", "api_key_encrypted", "key_hash", "key_encrypted", "password_hash", "password",
    "n8n_execution_id", "n8n_account_config", "payload", "webhook_url", "provider_keys",
    "secret", "token", "access_token", "private_key", "stripe_payment_intent",
}

results: list[tuple[bool, str, str]] = []
calls = 0


def record(ok: bool, name: str, detail: str = "") -> None:
    results.append((ok, name, detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}{('  — ' + detail) if detail and not ok else ''}")


def rpc(url: str, token: str, method: str, params=None, ident=1):
    payload = {"jsonrpc": "2.0", "id": ident, "method": method}
    if params is not None:
        payload["params"] = params
    body = json.dumps(payload).encode()
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "content-type": "application/json",
            "user-agent": UA,
            "authorization": f"Bearer {token}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


def call(url: str, token: str, tool: str, args: dict | None = None, ident=100):
    """One tools/call. Counted so the audit-line delta can be compared exactly."""
    global calls
    calls += 1
    status, body = rpc(url, token, "tools/call", {"name": tool, "arguments": args or {}}, ident)
    result = body.get("result") or {}
    payload = result.get("structuredContent")
    if payload is None:
        text = (result.get("content") or [{}])[0].get("text")
        try:
            payload = json.loads(text) if text else None
        except json.JSONDecodeError:
            payload = None
    return status, bool(result.get("isError")), payload


def keys_of(value, out: set[str]) -> None:
    if isinstance(value, dict):
        for k, v in value.items():
            out.add(k.lower())
            keys_of(v, out)
    elif isinstance(value, list):
        for v in value:
            keys_of(v, out)


def audit_lines(path: str) -> list[dict]:
    with open(path, "r", encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--owner-token", required=True)
    ap.add_argument("--second-token", required=True)
    ap.add_argument("--audit", default="/opt/swift/workflowswift/data/mcp-audit.log")
    args = ap.parse_args()
    url, audit = args.url, args.audit

    # ── the tool surface ────────────────────────────────────────────────────────────────────────
    status, body = rpc(url, args.owner_token, "tools/list", ident=1)
    tools = (body.get("result") or {}).get("tools") or []
    names = sorted(t.get("name") for t in tools)
    record(status == 200 and names == TOOLS, "tools/list is exactly the ten reads", f"{names}")
    record(
        all((t.get("annotations") or {}).get("readOnlyHint") is True for t in tools),
        "every tool is annotated read-only",
    )
    described = {t["name"]: t.get("description") or "" for t in tools}
    record(
        all("excerpt" in described[n].lower() for n in ("workflowswift_get_run",)),
        "get_run says its result is an excerpt",
        described.get("workflowswift_get_run", ""),
    )

    # ── tenancy: two workspaces, disjoint rows ─────────────────────────────────────────────────
    before = len(audit_lines(audit)) if os.path.exists(audit) else 0

    st_o, err_o, owner_wf = call(url, args.owner_token, "workflowswift_list_workflows")
    st_s, err_s, second_wf = call(url, args.second_token, "workflowswift_list_workflows")
    record(st_o == 200 and not err_o and st_s == 200 and not err_s, "both workspaces read workflows")
    owner_ids = {w["id"] for w in (owner_wf or {}).get("items", [])}
    second_ids = {w["id"] for w in (second_wf or {}).get("items", [])}
    record(
        (owner_wf or {}).get("count") != (second_wf or {}).get("count"),
        "second workspace sees a DIFFERENT count",
        f"owner={(owner_wf or {}).get('count')} second={(second_wf or {}).get('count')}",
    )
    record(bool(owner_ids) and not (owner_ids & second_ids), "the two workspaces' workflow ids are disjoint")

    _, _, owner_runs = call(url, args.owner_token, "workflowswift_list_runs", {"limit": 5})
    _, _, second_runs = call(url, args.second_token, "workflowswift_list_runs", {"limit": 5})
    record((owner_runs or {}).get("count", 0) > 0, "owner sees runs", f"{owner_runs}")
    record((second_runs or {}).get("count", 0) == 0, "second workspace sees no runs of the owner's",
           f"{second_runs}")

    # ── 404-not-403, both directions in one run id ──────────────────────────────────────────────
    run_id = ((owner_runs or {}).get("items") or [{}])[0].get("id")
    record(bool(run_id), "owner's first run id resolved")
    if run_id:
        st, is_err, payload = call(url, args.owner_token, "workflowswift_get_run", {"id": run_id})
        record(st == 200 and not is_err and (payload or {}).get("run", {}).get("id") == run_id,
               "OWNER reads that run")
        st_f, is_err_f, payload_f = call(url, args.second_token, "workflowswift_get_run", {"id": run_id})
        record(st_f == 200 and is_err_f and (payload_f or {}).get("error") == "not_found",
               "FOREIGN workspace gets not_found for the same run id",
               f"status={st_f} payload={payload_f}")
        st_x, is_err_x, payload_x = call(
            url, args.owner_token, "workflowswift_get_run",
            {"id": "00000000-0000-0000-0000-000000000000"})
        record(is_err_x and (payload_x or {}).get("error") == "not_found",
               "a missing run id answers the SAME not_found", f"{payload_x}")

    # a foreign WORKFLOW id must answer not_found too
    if owner_ids:
        some_wf = sorted(owner_ids)[0]
        st, is_err, payload = call(url, args.second_token, "workflowswift_get_workflow", {"id": some_wf})
        record(is_err and (payload or {}).get("error") == "not_found",
               "foreign workflow id answers not_found", f"{payload}")

    # ── all ten tools answer for the owner, and nothing secret is projected ────────────────────
    payloads = {"workflowswift_list_workflows": owner_wf, "workflowswift_list_runs": owner_runs}
    for tool in TOOLS:
        if tool in payloads:
            continue
        tool_args = {}
        if tool == "workflowswift_get_run" and run_id:
            tool_args = {"id": run_id}
        elif tool == "workflowswift_get_workflow" and owner_ids:
            tool_args = {"id": sorted(owner_ids)[0]}
        elif tool.startswith("workflowswift_get_"):
            continue
        st, is_err, payload = call(url, args.owner_token, tool, tool_args)
        payloads[tool] = payload
        record(st == 200 and not is_err, f"{tool} answers for the owner", f"{payload}")
    all_keys: set[str] = set()
    keys_of(list(payloads.values()), all_keys)
    leaked = sorted(all_keys & FORBIDDEN_KEYS)
    record(not leaked, "no payload projects a credential or an internal id", f"{leaked}")
    long_text = [
        v
        for p in payloads.values()
        for v in [json.dumps(p)]
        if len(v) > 200_000
    ]
    record(not long_text, "no single payload exceeds 200 KB")

    # ── audit ─────────────────────────────────────────────────────────────────────────────────
    record(os.path.exists(audit), "audit file exists", audit)
    if os.path.exists(audit):
        mode = stat.S_IMODE(os.stat(audit).st_mode)
        record(mode == 0o600, "audit file is 0600", oct(mode))
        lines = audit_lines(audit)
        delta = len(lines) - before
        record(delta == calls, "exactly one audit line per tool call", f"delta={delta} calls={calls}")
        tool_lines = [l for l in lines if l.get("tool") != "(auth)"]
        record(
            all({"ts", "door", "subject", "role", "tool", "args", "outcome"} <= set(l) for l in tool_lines),
            "every audit line carries its full shape",
        )
        blob = json.dumps(lines)
        tokens = [args.owner_token, args.second_token]
        record(
            not any(t in blob for t in tokens)
            and not any(t[:16] in blob for t in tokens),
            "ZERO token strings (not even a prefix) in the audit",
        )
        record(
            all(l.get("outcome") in {"ok", "denied", "unknown_tool"} or str(l.get("outcome", "")).startswith("tool_error")
                for l in tool_lines),
            "audit outcomes are from the known vocabulary",
        )
        print(f"      audit tail: {json.dumps(tool_lines[-3:], indent=None)[:600]}")

    failed = [r for r in results if not r[0]]
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
