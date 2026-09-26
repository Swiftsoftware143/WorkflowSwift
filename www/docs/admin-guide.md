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
`max_users`, `retention_days`, `can_export`, `can_deploy_n8n`, `has_api_access`) so both views of
a plan agree.

**The keys:**

- Numeric: `max_workflows`, `max_templates`, `max_instances`, `max_users`, `max_automations`,
  `max_integrations`, `max_api_keys`, `max_clients`, `max_portfolio`, `max_tags`,
  `max_industries`, `retention_days`
- On/off: `n8n_deploy`, `api_access`, `csv_export`, `webhook_export`, `custom_branding`,
  `google_sheets`, `priority_support`, `dedicated_support`, `sla_guarantee`, `audit_logs`,
  `custom_reports`

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
| `/api/v1/admin/feature-definitions` | GET | The 22 canonical feature keys, their value type, default and category |

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
| `/api/v1/admin/accounts/{id}` | DELETE | Delete the account and its data (cascades) |
| `/api/v1/admin/accounts/{id}/retention` | PUT | Per-account retention override |
| `/api/v1/admin/usage` | GET | Usage dashboard — credits, executions, n8n status per account |
| `/api/v1/admin/impersonate` / `stop-impersonation` | POST | Support impersonation |

A tenant (`accounts`) carries the slug, branding (logo, primary/accent colour, custom domain,
footer), industry, retention days and **its own Hexomatic key**. Users (`users.aid`) belong to
one tenant; `users.role` is `admin` / `member` (`perm_is_super_admin` marks the platform
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

**Storage note (accurate as of this writing):** `provider_keys.api_key` is stored as plain text
in the database; only `api_keys.key_hash` (the WorkflowSwift-issued keys) is argon2-hashed. Treat
database access as secret access until at-rest encryption is added.

## Integration Center — CoreSwift

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/integrations/coreswift/status` | GET | Is a CoreSwift key connected, and its base URL |
| `/api/v1/integrations/coreswift/lists` | GET | Proxy the CoreSwift lists catalogue |
| `/api/v1/integrations/coreswift/push` | POST | Manual push of captured leads |
| `/api/v1/user-keys` | GET/POST | Per-user integration keys (+ `/{id}` DELETE, `/health-check`) |

Inbound: `POST /api/v1/incoming` (internal key) is the single endpoint every Swift tool pushes
to — WorkflowSwift matches the payload to an active workflow, creates an instance and steps
through it, dispatching to integration targets and n8n.

MultiDirectory referral events can also arrive this way, e.g.
`{ "event": "referral_verified", "referrer_email": "...", "referee_email": "...", "zaarcash_earned": 100 }`,
and a workflow can turn them into notifications or CRM updates.

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
| `/api/v1/dashboard/workspace`, `/stats`, `/timeline`, `/widgets`, `/activity` | GET | Dashboard data |
| `/api/v1/dashboard/push-widget-data` | POST | Ingest custom metrics |

## Credits, payments and affiliates

- **1 credit per execution**; a run without credits is refused.
- Checkout: `/api/v1/checkout/create`, `/checkout/sessions`; providers configured per plan via
  `/api/v1/payment-providers`. Webhooks: `POST /api/v1/webhooks/stripe`,
  `POST /api/v1/webhooks/paypal` (signature-verified in the handler).
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
- Invoices: `/api/v1/invoices` and `/api/v1/invoices/{id}` read `invoices`; `amount` is
  `NUMERIC(10,2)` and is returned as a decimal **string** (`amount::text`), like `plan_tiers`
  prices — sqlx cannot decode NUMERIC into a JSON value.

## Rate limiting

Protected endpoints pass through `rate_limit` middleware keyed on the account. Exceeding it
returns `429` with a `Retry-After` header.

## Operational notes

- The container runs with `network_mode: host` and publishes nothing — the API binds
  `127.0.0.1:8085` on the host, behind nginx/Cloudflare.
- The binary is **image-baked** (no bind mount): restarting the container re-runs the same
  binary. Deploy with `/opt/swift/bin/deploy-workflowswift.sh`, which rebuilds the image,
  force-recreates the container and verifies sha256 parity against the repo build.
- `sqlx::migrate!` embeds migrations in the binary, so a new `migrations/*.sql` file only takes
  effect at the next deploy — read it before it can run.
