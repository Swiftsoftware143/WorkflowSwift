# Admin Guide — WorkflowSwift

Audience: the **super admin** (the platform operator). Everything under `/api/v1/admin/*`
requires a super-admin JWT — `perm_is_super_admin` is checked at the API, so a tenant admin or
member gets `403`, not a hidden button.

Base: `https://workflowswift.com/api/v1` · Health: `GET /api/v1/health` (public).

## Plan limits — settable AND enforced

This is the part of the platform that controls revenue, so it is worth stating exactly how it
works.

**One source of truth.** A plan's limits live in `plan_tiers`, in the `features` JSONB under the
**canonical key names**, mirrored into the legacy dedicated columns (`max_workflows`,
`max_users`, `retention_days`, `can_deploy_n8n`, `has_api_access`) so both views of
a plan agree.

**The keys:**

- Numeric: `max_workflows`, `max_templates`, `max_instances`, `max_users`, `max_automations`,
  `max_integrations`, `max_api_keys`, `max_clients`, `max_portfolio`, `max_tags`,
  `max_industries`, `retention_days`
- On/off, **enforced by a gate**: `n8n_deploy`, `api_access`
- On/off, **support-tier promises** (not code gates — see below): `priority_support`,
  `dedicated_support`, `sla_guarantee`

**Retired keys (kanban t_1aa78926).** `csv_export`, `webhook_export` and `google_sheets` used to be
settable per plan and are gone — from `plan_tiers.features`, from the plan payloads and from
`src/features.rs::BOOLEAN_FLAG_KEYS` (migration `074_retire_export_entitlements.sql`). All three were
`true` on every plan including Free and **no code path in the crate honoured any of them**: there is
no CSV writer/store/download, no export contract to gate, and no Google credential, OAuth flow,
provider preset or destination row. A plan that advertises an entitlement the app cannot deliver is
the defect, not a feature. A flag belongs here only together with the gate that enforces it.

**Retired keys (kanban t_413b4aab).** `custom_branding`, `audit_logs` and `custom_reports` are gone
the same way (migration `075_retire_unbacked_plan_flags.sql`). They were off on Free but on from a
paid tier up, and each named a software capability the crate does not have:

- `custom_branding` — the `accounts` branding columns (`logo_url`, `branding_name`,
  `primary_color`, `accent_color`) have no writer, no reader and no renderer, and this app has no
  per-tenant public page to white-label in the first place. **The columns themselves were then
  dropped** (`077_drop_account_branding_columns.sql`, kanban t_731bf864), together with the equally
  unused `custom_domain`; they had 0 non-null values across all 8 accounts.
- `audit_logs` — the `audit_logs` table holds **0 rows** and is written by nothing; its only
  reader (`GET /dashboard/activity`) was deleted by kanban t_3a8ccd2a.
- `custom_reports` — there is no report table, handler, route or console surface anywhere.

Removing them changed nothing a tenant could receive: no console renders these key names, the
tenant console reads `GET /api/v1/plans` for **name and price only**, and the marketing site never
names them. Do not re-add one without both the gate that enforces it and the surface that delivers it.

**Support-tier promises (kanban t_413b4aab) — deliberately NOT gated.** `priority_support`
("Priority email and chat support"), `dedicated_support` ("Dedicated account manager") and
`sla_guarantee` ("Service level agreement guarantee") have **no code path and are not supposed to**:
their mechanism is the support process, and which tier promises what is a pricing/support-contract
decision, not an engineering one. They stay in `plan_tiers.features` and
`plan_feature_definitions`, are named here on purpose, and are pinned by the
`boolean_keys_are_either_gated_or_documented_support_promises` test in `src/features.rs`, so the
next "a flag no gate reads" census does not re-flag them. If a future pass wants them *removed*,
that is a pricing decision to make first.

**Semantics:** `-1` or the string `"unlimited"` means unlimited; `0` means "not included in this
plan" (any attempt returns `402 Payment Required`); any other number is a hard cap and the
boundary is refused with `402`. A key that is absent is treated as *not configured* and allowed.

**Endpoints:**

| Endpoint | Method | Notes |
|---|---|---|
| `/api/v1/admin/plans` | GET | List plan tiers with their limits |
| `/api/v1/admin/plans` | POST | Create a plan — accepts the limit keys **either at the top level or inside `features`** |
| `/api/v1/admin/plans/{id}` | PUT | Update a plan — limits are **merged**, so saving one field never wipes the others |
| `/api/v1/admin/plans/{id}` | DELETE | Delete a plan |
| `/api/v1/admin/feature-definitions` | GET | The 16 canonical feature keys, their value type, default and category |

**Where enforcement happens:** `src/features.rs` resolves the account's plan
(`account_plans` → `accounts.plan_id` → lowest-`sort_order` active tier, so an account with no
plan falls back to the free tier rather than being unlimited) and applies
`enforce_feature_limit` / `enforce_plan_flag` at the API. Wired gates include: workflow creation
(`max_workflows`), templates (`max_templates`), instance creation (`max_instances`), team growth
(`max_users`), automations, integration targets, API-key creation (`max_api_keys` + `api_access`),
tags, portfolio companies, industries, plan creation, and **n8n deployment** (`n8n_deploy`).

**Default tiers as seeded:**

| Tier | Workflows | Users | Templates | Instances | API keys | Clients | Ret. days | n8n | API |
|---|---|---|---|---|---|---|---|---|---|
| Free | 3 | 2 | 3 | 10 | 1 | 5 | 30 | – | – |
| Starter | 15 | 10 | 10 | 100 | 3 | 25 | 30 | ✓ | ✓ |
| Professional | unlimited | 25 | 25 | 1000 | 10 | 100 | 90 | ✓ | ✓ |
| Enterprise | unlimited | unlimited | unlimited | unlimited | unlimited | unlimited | 365 | ✓ | ✓ |

## Accounts and tenants

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/admin/accounts` | GET | List all accounts (tenant) |
| `/api/v1/admin/accounts/create` | POST | Create an account |
| `/api/v1/admin/accounts/{id}` | DELETE | Delete the account and its data (cascades). The account's n8n `WFS <uuid>` workflow mirrors are retired **first**; if they cannot be retired (n8n unreachable or refusing) the whole delete is **refused with 502** — an orphaned mirror is permanent, while a retryable 502 is not. |
| `/api/v1/admin/accounts/{id}/retention` | PUT | Per-account retention override |
| `/api/v1/admin/usage` | GET | Usage dashboard — credits, executions, n8n status per account |
| `/api/v1/admin/impersonate` / `stop-impersonation` | POST | Support impersonation |

A tenant (`accounts`) carries ONE identifier: `account_slug`, NOT NULL UNIQUE (`tenants_slug_key`),
written by registration and the portfolio writers and read by the bridge and by this console's
account list. A second, nullable `slug` column used to sit beside it and was **dropped** on
2026-10-02 (migration 078, kanban t_27bd3765): no UNIQUE, no index, no view, no reader in the fleet,
6 of 8 rows NULL — and on the 2 rows that carried a value it was an exact copy of `account_slug`, so
`PUT /api/v1/accounts {"slug": "..."}` answered 200 while renaming a field nothing read.
`accounts` also carries footer text (`footer_year` / `footer_company`, writable via
`PUT /api/v1/accounts`), industry, retention days and **its own Hexomatic key**. The
`logo_url` / `branding_name` / `primary_color` / `accent_color` branding columns and the unused
`custom_domain` were **dropped** on 2026-10-02 (migration 077, kanban t_731bf864): nothing in this
app wrote or rendered them, so the schema no longer advertises white-label branding. The plan flag
that sold it (`custom_branding`) had already been retired by kanban t_413b4aab. Users (`users.aid`)
belong to one tenant; `users.role` is `admin` / `member` (`perm_is_super_admin` marks the platform
operator).

## Admin settings, retention and email

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/admin/settings` | GET | All settings |
| `/api/v1/admin/settings/{key}` | GET/PUT | Read/update one setting (secret values come back **masked**) |
| `/api/v1/admin/settings/email/test` | POST | Send a real test email |
| `/api/v1/admin/retention` | GET/PUT | Platform retention policy |
| `/api/v1/admin/email-templates` | GET/POST | Message templates |
| `/api/v1/admin/email-templates/{id}` | PUT/DELETE | Edit/delete a template |
| `/api/v1/admin/site` | GET/PUT | Public site copy |
| `/api/v1/admin/industry-sources` | GET/POST | Industry data sources (+ `/seed`, `/{id}` DELETE) |

Email credentials are read from the **database only** — there is no environment fallback — and
the provider is an admin choice (`smtp`, `mailgun`, `sendgrid`, `sendiio`).

## Workspaces, agents and tickets

Users create **workspaces** within their tenant (the `portfolio_companies` table); a workspace carries
a name and a slug, `POST` takes an optional `industry_slug` that seeds that workspace's dashboard, and
`GET /api/v1/agents?workspace_id={id}` scopes the agent list to one workspace. **Provider keys are
per tenant, never per workspace** — see BYOK below.

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/workspaces` | GET/POST | List/create workspaces |
| `/api/v1/workspaces/{id}` | DELETE | Delete a workspace |
| `/api/v1/agents?workspace_id={id}` | GET | Agents in a workspace |
| `/api/v1/tickets` | GET/POST | Ticket list / create |

**Paperclip** is the agent-orchestration layer *above* WorkflowSwift (task assignment, budgets,
execution history). It is not a WorkflowSwift user-facing feature and nothing in the user sidebar
exposes it — WorkflowSwift only hands work off and receives results.

## BYOK — provider keys

Keys the **customer** brings (OpenAI, Resend/SendGrid, social, CoreSwift, …) are stored in
`provider_keys` **per tenant** (`aid`), one row per (tenant, provider), entered in the app UI.

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/provider-keys` | GET | List configured providers — **values masked** |
| `/api/v1/provider-keys` | POST | Save/update a tenant provider key |
| `/api/v1/provider-keys/{provider}` | DELETE | Remove a provider key |
| `/api/v1/provider-keys/{provider}/test` | POST | Live connection probe |
| `/api/v1/provider-presets`, `/available-providers` | GET | Preset catalogue (public) |

Every read endpoint returns the key masked (`sk-…161`); the raw value is never returned. There is
no env-var-only provider and no single global admin paste — if a provider has no per-tenant key,
the related feature is simply "not connected".

**Storage note (measured 2026-10-02, kanban t_c603a937):** `provider_keys.api_key` is **ciphertext
at rest** — `enc:v1:` + base64, AES-256 via pgcrypto, master key only in the process environment
(`PROVIDER_KEY_ENC_SECRET`); migration `048` arms it, a DB CHECK refuses a value without the prefix,
and a write fails closed when the master key is absent. `api_keys.key_hash` (the WorkflowSwift-issued
keys) is argon2-hashed; `integration_targets.api_key` uses the same envelope (migration `053`).

## Integration Center — CoreSwift

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/integrations/coreswift/status` | GET | Is a CoreSwift key connected, and its base URL |
| `/api/v1/integrations/coreswift/lists` | GET | Proxy the CoreSwift lists catalogue |
| `/api/v1/integrations/coreswift/push` | POST | Manual push of captured leads |
| `/api/v1/user-keys` | GET/POST | Per-user integration keys (+ `/{id}` DELETE, `/health-check`) |

The per-user `user_integrations` family (`GET`/`POST /api/v1/integrations`, `/native`,
`/native/{provider}`, `DELETE /{provider}`, `/health-check`) and the console's "My Integrations"
panel were RETIRED on 2026-10-02 (kanban t_cb839034): that store had 0 rows and no delivery path
read it — every delivery path reads `provider_keys` — so those paths answer 404, and the table is
dropped by migration `076`. `GET /api/v1/integrations/resolve` (the step-provider resolver) and the
CoreSwift spoke above are unaffected.

Inbound: `POST /api/v1/incoming` (internal key) is the single endpoint every Swift tool pushes
to — WorkflowSwift matches the payload to an active workflow, creates an instance and steps
through it, dispatching to integration targets and n8n. A workflow only dispatches to a target when
its step carries a seeded `integration_target_id` (see below).

MultiDirectory referral events can also arrive this way, e.g.
`{ "event": "referral_verified", "referrer_email": "...", "referee_email": "...", "zaarcash_earned": 100 }`,
and a workflow can turn them into notifications or CRM updates.

## Integration targets & step dispatch — operator-provisioned

Measured 2026-10-02 (kanban t_97a0bd3f). Targets exist and dispatch works; the **binding** has no
shipped writer, so it is provisioned server-side.

- **Targets** (`integration_targets`, aid-scoped) are created/edited in the admin console under
  **Integrations → Integration Targets** (`GET/POST /api/v1/integration-targets`, `PUT/DELETE
  /api/v1/integration-targets/{id}`). Each row carries `webhook_url`, a `provider_preset`,
  `allowed_domains` and `daily_limit`; `webhook_security::check_webhook_security` enforces the
  domain allowlist and the daily cap, counting rows in `delivery_log`.
- **The URL source** is the target's `webhook_url` or, when that is blank, its `provider_preset` — a
  key in `integration_provider_presets`, served by `GET /api/v1/provider-presets`
  (8 rows). The console's **Integration Targets** panel offers the catalogue as a SELECT, the create
  and update routes validate the key against it (an unknown key is a field-level 422, never a
  23503), and both accept `provider_preset` — a string sets it, `null` clears it. `forward_dispatch`
  appends the payload's `path` to the preset's `base_url`, and the dispatch security gate validates
  that same effective URL, so a preset-routed target needs no webhook URL at all (kanban t_cb839034
  wired this half of the routing contract: until then the read was live while 5/5 rows had no writer
  for the column).
- **The binding** is the column `workflow_steps.integration_target_id`.
  The executor reads it at `src/execution.rs` in the arm
  `"integration" | "integration_dispatch"`, i.e. **a step dispatches only if its `step_type` is
  `integration`**. Neither the Builder (15 types) nor the app's own `POST
  /api/v1/workflows/validate-steps` vocabulary (25 types) contains that type, and the steps API
  (`POST/PUT /api/v1/workflows/{id}/steps`) accepts no `integration_target_id` field — a request
  carrying one is accepted and the value is dropped. Seed it with SQL; there is no UI to bind a step
  and none is wanted until a tenant can create its own targets. Migration `072` retired migration
  018's other step columns (`api_path` / `api_method` on both step tables, and this table's
  `integration_target_id` twin): measured 0 readers, 0 writers, 0 non-default rows, and installing a
  template copies only `step_type/name/description/sort_order/config` into `workflow_steps`.
- **Credential:** `forward_dispatch` sends `Authorization: Bearer <key>` + `x-api-key: <key>` from,
  in order, **(1)** the credential stored ON THE TARGET ROW (`integration_targets.api_key`, ciphertext
  at rest; set at create, rotated/cleared via `api_key` on `PUT /api/v1/integration-targets/{id}`),
  else **(2)** the account's `provider_keys` row. A target credential that cannot be decrypted fails
  the dispatch (5xx, reason in `delivery_log`) instead of falling back to the account key; with
  neither set the POST goes out unauthenticated. Before 2026-10-02 the target-row `api_key` was read
  by no path at all, so a target created with one still dispatched with no auth header (kanban
  t_c603a937). The account row's `metadata.auth_type` still picks the `basic`/`x-api-key` shape, and
  a `_forward_auth` string in the payload body overrides the Authorization header last.
- **One-shot dispatch:** `POST /api/v1/integration-dispatch?target_id={id}` forwards one JSON body to
  the target (auth'd route; every attempt is counted in `delivery_log`). The legacy
  `n8n-templates/*.json` name `/api/integration-dispatch` (no `/v1`) — the API is mounted at
  `/api/v1`, so those URLs are unrouted; n8n flows must use `/api/v1/integration-dispatch`.

## Affiliate product auto-sync

Plan changes sync to FunnelSwift's `affiliate_products`: create → `action: create`, update →
`action: update`, delete → `action: deactivate`. The sync is asynchronous and needs
`FUNNELSWIFT_URL` (default `http://localhost:8080`).

## Industries and dashboards

The platform ships **6 industries**: site-flipping, e-commerce, saas, government-contracting,
real-estate, marketing-agency. Each account gets a dashboard tab per selected industry, seeded
with Data Cards; industry data sources can be `api`, `webhook`, `rss` or `scraper` and cost
credits per call.

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/industries` | GET | List industries (public) |
| `/api/v1/accounts/industry` | GET/PUT | Account industries (GET lists linked; PUT sets primary) |
| `/api/v1/accounts/industry/{slug}` | DELETE | Remove an industry dashboard from the account |
| `/api/v1/dashboard/tabs` | GET | Tab-navigated dashboard (per-industry tabs + widgets) |
| `/api/v1/dashboard/workspace`, `/stats`, `/timeline`, `/widgets` | GET | Dashboard data — `/stats` totals are rendered on the dashboard (workflows / clients / templates) |
| `/api/v1/dashboard/push-widget-data` | POST | Ingest custom metrics |

## Credits, payments and affiliates

- **1 credit per execution**; a run without credits is refused.
- Checkout: `/api/v1/checkout/create`, `/checkout/sessions`; providers configured per plan via
  `/api/v1/payment-providers`. Webhooks: `POST /api/v1/webhooks/stripe`,
  `POST /api/v1/webhooks/paypal`.
- `POST /api/v1/webhooks/paypal` is **signature-verified before anything is written or dispatched**
  (kanban t_5cf44e1b). The four PayPal signature headers (`paypal-transmission-id`,
  `paypal-transmission-time`, `paypal-transmission-sig`, `paypal-cert-url`) are required and the
  signature is checked against PayPal's `verify-webhook-signature` API, authenticated with the REST
  `client_id:client_secret` and verified against `PAYPAL_WEBHOOK_ID` (or the `webhook_secret` of the
  active `paypal` provider row — admin console -> Payment providers, no redeploy). Fail-closed
  replies, in order: `401 missing_paypal_signature_headers`, `503 paypal_not_configured` (no webhook
  id or no credential: PayPal is **not** called and nothing is written),
  `401 paypal_verification_api_error` / `401 paypal_verification_unreachable` (the verdict itself
  could not be obtained), `401 signature_verification_failed` (a real FAILURE verdict). Only a
  verified event reaches `payment_webhook_events` and fulfilment.
- Affiliate attribution is owned by FunnelSwift, not by this app: WorkflowSwift stores no affiliate
  records and exposes **no** `/api/v1/affiliates` endpoint (the auto-generated stub that answered
  500 was deleted — kanban t_01fa9bbc). A paid plan upgrade notifies
  `POST {FUNNELSWIFT_URL}/api/v1/internal/affiliate/upgrade-event` (key-authenticated) and the
  referral is credited there — that is the only conversion this app reports. The keyless
  `POST {FUNNELSWIFT_URL}/api/v1/webhooks/conversion` post was **deleted** (kanban t_f6eb8834): it
  sent no `X-Internal-Key` and no attribution, so the now key-gated receiver refused it `401` and
  the conversion was lost silently; re-adding it would only double-pay, because the upgrade event
  above already credits the same purchase.
- Per-account registries backed by real tables: `/api/v1/tag-groups` (Tags & Labels -> Tag Groups,
  `tag_groups`, migration 054) and `/api/v1/webhooks` (Communications -> Webhooks, `webhooks`,
  migration 055). The `webhooks` table is the tenant's own endpoint registry — it is not the
  inbound `stripe`/`paypal` receivers above, whose event log is `payment_webhook_events`.
- Email templates are **platform-wide, not per-account**: the only create/read/update/delete path is
  `/api/v1/admin/email-templates` (Admin -> Email Templates, super-admin only — the token must carry
  `perm_is_super_admin`), and `src/email.rs` resolves a template with no `aid` filter
  (`template_type = $1 AND (is_default = true OR is_default IS NULL)`). The tenant-scoped
  `/api/v1/email-templates` family and its Communications -> Email Templates panel were **deleted**
  (kanban t_00e7b709): every arm was dead — its INSERT omitted the NOT NULL
  `template_type`/`subject`, so it answered `500 Database error` for every authenticated caller and
  wrote no row; its reads decoded the NULL system `aid` as a UUID, so list answered
  `200 {"items":[],"count":0}` over a table with rows and get/put answered 500; and its DELETE
  removed a system-wide template for any authenticated caller. Do not re-add a second create path.
- Invoices: `/api/v1/invoices` and `/api/v1/invoices/{id}` read `invoices`; `amount` is
  `NUMERIC(10,2)` and is returned as a decimal **string** (`amount::text`), like `plan_tiers`
  prices — sqlx cannot decode NUMERIC into a JSON value.

## Rate limiting

Protected endpoints pass through `rate_limit` middleware keyed on the account. Exceeding it
returns `429` with a `Retry-After` header.

A second, `pre_auth_rate_limit` guard runs **before** any credential is verified, for requests
that present an API key (`workflowswift_...`): it exists so a flood of requests that merely look
like a key cannot drive the expensive Argon2 hash check, and its `429` arrives without the key
ever being looked up (kanban t_da8a579a). It is keyed on the client address the PROXY vetted —
nginx's `X-Real-IP` (`$remote_addr`: the real visitor behind Cloudflare, the true peer for a
caller that reaches the origin directly) — never on a header the caller can hand-set. A request
with no address header at all shares one `unknown` bucket, so omitting the header is not a way
around the limit.

## Operational notes

- The container runs with `network_mode: host` and publishes nothing — the API binds
  `127.0.0.1:8085` on the host, behind nginx/Cloudflare.
- The binary is **image-baked** (no bind mount): restarting the container re-runs the same
  binary. Deploy with `/opt/swift/bin/deploy-workflowswift.sh`, which rebuilds the image,
  force-recreates the container and verifies sha256 parity against the repo build.
- `sqlx::migrate!` embeds migrations in the binary, so a new `migrations/*.sql` file only takes
  effect at the next deploy — read it before it can run.
