# WorkflowSwift — `accounts.is_active` is now a real suspension switch

**Card:** t_7f53bd68 (follow-up of the fleet security sweep t_fbd8fc03) · **Date:** 2026-11-24 ·
**App:** `/opt/swift/apps/WorkflowSwift` (container `workflowswift`, :8085, https://app.workflowswift.com) ·
**Deployed binary:** sha256 `7a808386db754bec` (container `de2d35b2b1d6`, repo == container == /proc/1/exe)

## The finding, re-measured

`accounts.is_active` was written exactly once (signup) and read by nothing. Both credential paths
tested only that the account ROW EXISTS:

* `src/auth/middleware.rs::account_is_live()` ran `SELECT EXISTS(... accounts WHERE id = $1)`;
* the MCP door (`src/mcp/mod.rs` → `auth::api_key_auth::authenticate`) never touched `accounts` at all.

So a key whose account row carried `is_active = false` still answered 200 with the full tool list, and
the admin console painted an Active/Inactive badge from a flag no code could act on. Nothing wrote
`false`, so it was a latent gap — but an ambiguous column an operator is told to trust.

## Decision: make it real (not delete the pretence)

Reasons: (a) an operator needs to stop an abusive or non-paying workspace WITHOUT destroying its data
(closure/purge already exists as a separate, deliberate action); (b) a column the console displays is
a promise — deleting it would leave "Active" meaningless; (c) all 12 live accounts were `is_active =
true`, so enforcing it broke nobody (measured before the change).

## What changed (commit `4942ecd`, 8 files)

| File | Change |
|---|---|
| `src/auth/middleware.rs` | `account_is_live()` reads `is_active` instead of `EXISTS(...)`: a suspended OR missing account is 401 on every JWT request, next request included. |
| `src/auth/api_key_auth.rs` | `authenticate()` refuses a suspended/missing account (401) right after the owning-user check and before the plan gate. This is also the MCP door's resolver, so the door now honours the switch with no second implementation. |
| `src/auth/handlers.rs` | `login` refuses a suspended account with `403 "This account is suspended. Contact support if you need it reactivated."` (the password is already proved at that point, so naming the state leaks nothing). Without this the customer would get a token that fails on the next request. |
| `src/handlers/admin_settings_handler.rs` | new `admin_set_account_active` — `PUT /api/v1/admin/accounts/{id}/active` `{is_active: bool}`, super-admin only, idempotent both ways. Refuses (403) the account you are signed in as, a portfolio company, and this app's own workspace (`account_slug = 'workflowswift'`) — the same three the delete arm keeps; 404 unknown, 400 malformed body. Logs `warn` with actor + account id (never a credential). |
| `src/routes.rs` | mounts the route in the protected admin router. |
| `src/execution_worker.rs` | the due-step query joins `accounts` and requires `a.is_active`: a suspended account's queued `delay`/`wait` steps stop advancing and stay PENDING (not failed), so reactivation resumes the queue where it was. |
| `www-admin/index.html` | per-row **Suspend / Reactivate** control on Admin → Accounts (portfolio rows get none), confirmation on suspend, reload after the write. |
| `www-admin/admin-guide.html` | documents the route, the three refusals, the pending-step behaviour and the control. |

Sessions do not need a `sessions_valid_from` bump: every request re-reads the account row.

## Live proof (both ways)

`scratch/suspension-proof-t_7f53bd68.py` — **30/30** against https://app.workflowswift.com on a probe
account with a real Argon2 password, a real sha256 API key and a real session JWT:

* active: login 200, `/auth/me` 200, MCP `tools/list` 200 with 10 tools;
* refusals: suspend self → 403, portfolio company → 403, own workspace → 403, unknown → 404, no body → 400, customer session → 403;
* suspend → 200 `is_active=false` (read back from the column);
* suspended: MCP → **401**, a JWT minted BEFORE the suspension → **401**, `/workflows` → **401**, login → **403** "suspended", while the key row is still `is_active = true` (the refusal is the account, not revocation), and the operator's own session still 200;
* reactivate → 200; MCP 200/10 tools, session 200, login 200;
* other door checks untouched: revoked key 401, expired key 401, deactivated owning user 401, all restored → 200;
* served console carries the control and the served guide documents it;
* cleanup: 0 probe accounts/users, 12/12 real accounts active.

`scratch/worker-suspension-proof-t_7f53bd68.py` — **7/7**: first asserts the SQL under test is the
worker's own statement (clause by clause, because the source carries SQL comments), then a probe
account + client + workflow + instance + one due `delay` step: selected while active → invisible while
suspended (step still `pending`) → selected again after reactivation. Zero residue.

`scratch/console-suspension-proof-t_7f53bd68.py` (Puppeteer + Chromium) — **10/10**: an operator
session is minted (HS256 with the app's own `JWT_SECRET`, no password involved), the Admin → Accounts
screen is driven, **Suspend** is pressed (confirmation shown), the DB column flips to false and the row
reads Inactive; **Reactivate** flips it back and the row reads Active. Screenshots:
`scratch/wfs-console-suspended-t_7f53bd68.png`, `scratch/wfs-console-reactivated-t_7f53bd68.png`.

## Boundary kept

Suspension stops every credential-based path and the app-side queue. A workflow already EXECUTING
inside n8n, and callbacks that authenticate with the INSTALL's internal key, are not credential-scoped
to a tenant — unchanged from before this card, and out of scope.

## Operator note

The switch is Admin → Accounts → **Suspend** / **Reactivate**, or
`PUT /api/v1/admin/accounts/{id}/active {"is_active": false|true}` with a super-admin session.
