# User Guide — WorkflowSwift

WorkflowSwift turns a captured lead or form entry into automated, multi-step follow-up: an
incoming webhook creates an instance, the engine walks the steps, and each step can call an AI
provider, send an email, export to a CRM, notify someone, wait, or fork.

Base API: `https://workflowswift.com/api/v1` — all non-public endpoints need
`Authorization: Bearer <token>` from `POST /auth/login`. Health: `GET /api/v1/health`.

## Getting started

1. Register at `https://workflowswift.com/register`. Your **organization (tenant) is created
   automatically** — it carries the slug, the branding settings (logo, primary/accent colour,
   custom domain, footer company/year) and your **Hexomatic key**.
2. You land on a dashboard pre-configured for the **industry** you picked at signup, with named
   **Data Cards** (dashboard widgets) already seeded.
3. Invite team members (`POST /users/invite`) — they join **your** tenant, with their own
   password and role (`admin` / `member`).

## Tenancy, users and roles

- One tenant = one business account. Users, workflows, clients, leads, API keys and provider
  keys all belong to the tenant; reads are scoped to it server-side.
- `GET /users` returns only your tenant's users; `GET /users/team` lists team members.
- The **Admin area is only reachable by a super admin.** Everyone else gets `403` from
  `/api/v1/admin/*`, enforced at the API (not just hidden in the UI).

## Workflows

- **Create**: from scratch or from the template gallery — `Templates Gallery` (system) and
  `My Workflows` (your own / cloned).
- **Steps**: add, edit (name, description, config), reorder, delete. A step's **type is fixed
  when you create it** — to change the type, delete the step and add a new one.
- **Step 1 is a Data Card step**: it picks a dashboard widget **by name** (e.g. "Orlando
  Plumbers") from the widgets in your dashboard — nothing is hardcoded.
- **Deploy to n8n** (plan-gated), **run manually**, and every run creates an **instance** with
  traceable state, a step-by-step history and full execution logs.
- A template that is **not included in your plan is locked** — the lock is enforced server-side,
  so the padlock badge is not just decoration.
- **Credits**: 1 credit per execution. Running out is refused by the API, not just warned about
  in the UI. Check `GET /credits/balance`.

### Builder

**Builder** in the sidebar (or the **Builder** button on a workflow row) is the step-configuration
surface. Pick a workflow, then:

- **+ Add Step** — the type locks once created, as above. A Data Card step lists your real
  dashboard widgets by name to choose from.
- **↑ / ↓** reorder the steps. A move that would leave a non-Data-Card step first is refused by
  the API and shown as an error — step 1 must be the Data Card.
- **Validate** checks the whole step list against the rules and lists errors/warnings inline.
- **Edit** changes the name, description and step config in place; **Del** removes the step.

## Renditions

Renditions are the media a workflow produces — a **Render Video / Image / Audio** step (or the
n8n mirror's log node) records each generated asset here.

- **Renditions** in the sidebar shows the tenant summary (active, expiring within 7 days, by asset
  type, by provider), the gallery, and a **per-workflow timeline** (pick the workflow in the filter).
- Select two or more assets and **Stitch selected** to group them into one parent rendition.
- The **status** filter switches between Active, Expired and All. Deleting a rendition retires it
  (status `expired`) instead of erasing it, so it stays retrievable until purged.

## Surfaces

A **Surface** is a named frontend/deployment target inside your tenant (website, mobile app,
chatbot, Alexa skill, kiosk): `GET/POST /surfaces` (name, slug, description).

- Surfaces are **defined only in the admin panel** — there is no standalone Surfaces page for
  regular users.
- Users instead get a **surface selector / filter** inside the editors, and workflows and leads
  can be tagged with a surface (`surface_id`).
- Today the surface filter is applied by **workflows** and **leads**; content, templates,
  exports and prospecting do not filter by surface yet.

## Dashboard: Data Cards, Brand Monitor, Competitor Watch

- **Data Cards** are named widgets. Workflow: Add Widget → name → type (prospecting / partners /
  contracts) → fill with data (search, import, or connect a source).
- **Brand Monitor** and **Competitor Watch are dashboard tabs, not workflows** — every user gets
  them; type a search term or competitor name and the tab renders the results.
- Data flow is `extension / API / n8n → dashboard display → optionally feeds a workflow`.
  Custom metrics come in through `POST /dashboard/push-widget-data` with a metric key.
- Your tenant has one tab per industry you have selected; the dashboard also exposes live stats,
  an activity timeline and per-workspace views.

## Keys: two different things

| | Where it comes from | What it is for |
|---|---|---|
| **API Keys** | `POST /api-keys` — generated by WorkflowSwift, shown **once**, stored hashed | Calling WorkflowSwift programmatically |
| **Provider Keys** | `POST /provider-keys` — **yours**, from OpenAI / Resend / SendGrid / social etc. | Letting WorkflowSwift call *your* third-party providers |

- **Bring your own keys (BYOK).** Every third-party key is entered by **you** in the app, at
  tenant level; it is never a global paste by an admin and never env-var-only.
- Provider keys are returned **masked** (`sk-…161`) by every read endpoint — the raw value is
  never sent back.
- **API keys authenticate.** Send the raw key exactly as it was shown once, as
  `Authorization: Bearer workflowswift_...` (the same `Authorization` header the login JWT uses).
  It is verified server-side (argon2) and resolved to the account and user that own it, so every
  call is tenant-scoped — `GET /bridge/status` echoes the account the *key* belongs to. The login
  JWT keeps working unchanged; a client may use either.
- A key honours the row it was minted with: `is_active = false` or an `expires_at` in the past is
  refused with `401`; `permissions` (`[]` = full account scope, `["read"]` = read-only, `"*"` =
  everything) is enforced per request, so a read-only key gets `403` on a write; `last_used_at` is
  stamped on every successful use. Minting is gated by your plan (`api_access`, `max_api_keys` ->
  `402` without them).
- **This is the credential the Chrome extension uses** (see *Chrome extension - Swift Market
  Intel* below): extension Options -> *API token*.

## Plans and limits

Plan limits are enforced **at the API** (`402 Payment Required` when you exceed them) and can be
set per plan by an admin: `max_workflows`, `max_templates`, `max_instances`, `max_users`,
`max_automations`, `max_integrations`, `max_api_keys`, `max_clients`, `max_portfolio`,
`max_tags`, `max_industries`, `retention_days`, plus the on/off features `n8n_deploy`,
`api_access`, `csv_export`, `custom_branding`, `webhook_export`, `google_sheets`,
`priority_support`, `dedicated_support`, `sla_guarantee`, `audit_logs`, `custom_reports`.
`-1` (or `unlimited`) means unlimited.

Your tier today:

| | Workflows | Users | Instances | API access | n8n deploy |
|---|---|---|---|---|---|
| Free | 3 | 2 | 10 | – | – |
| Starter | 15 | 10 | 100 | ✓ | ✓ |
| Professional | unlimited | 25 | 1000 | ✓ | ✓ |
| Enterprise | unlimited | unlimited | unlimited | ✓ | ✓ |

## Custom branding

Set logo, primary/accent colour, custom domain and footer text per tenant
(`PUT /accounts/{id}`) — available on plans that include `custom_branding`.

## Integrations

- **Integration Center** (`GET /integrations`): connect CoreSwift — inbound captured leads are
  pushed to CoreSwift contacts using **your** CoreSwift key, and captured workflows can push to a
  CoreSwift list. `GET /integrations/coreswift/status` reports whether your key is connected.
- **Integration targets** (`/integration-targets`) and **step integrations**
  (`/step-integrations`) configure where a step dispatches to.
- **Incoming webhook**: `POST /api/v1/incoming` is what other Swift tools push leads to. It is
  protected by an internal key.

## Chrome extension — Swift Market Intel

- **Download**: `https://workflowswift.com/api/v1/extension.zip` — this is a live route on the app
  (`GET /extension.zip`, no auth) that serves the build embedded in the running API binary, so the
  URL never points at a superseded release and can be linked from anywhere. The static copies
  (`/swift-market-intel-extension-<version>.zip` and the rolling alias
  `/swift-market-intel-extension.zip`) are CDN-cached mirrors — prefer the `/api/v1/` URL.
  Install guide: `/swift-market-intel-extension.html`. Version + sha256 of the current build:
  `/extension-latest.json` (rewritten by `/opt/swift/scripts/ws-publish-extension.sh` on release).
- **Install**: unpack the zip, then `chrome://extensions` → Developer mode → *Load unpacked* →
  pick the folder containing `manifest.json`. Sideload-only — not on the Chrome Web Store, so there
  is no "Add to Chrome" button and updates are manual.
- **Connect**: extension Options → paste your API token → leave **API Base URL** at
  `https://workflowswift.com/api/v1` → *Test Connection* → Save.
- **Collect**: open a supported listing → extension icon → *Scrape Current Page* →
  *Send to WorkflowSwift*. Supported: Etsy, Amazon, eBay, Facebook Marketplace, Shopify,
  Alibaba, AliExpress, Craigslist, Pinterest, TikTok, Instagram, Yelp, Google Maps.
- **Sending costs 1 credit** per run (charged to the account the key belongs to). A trigger aimed at
  a workflow that belongs to a *different* account is refused with `403`, an unknown one with `404`
  — the target is resolved against your own workflows first, so one tenant cannot fire another's.
- **Endpoints it uses** (all under `/api/v1`, `Authorization: Bearer <token>`):
  `GET /bridge/commands`, `GET /bridge/status`, `POST /workflows/trigger`,
  `POST /bridge/ingest`. Command acknowledgement (`POST /bridge/commands/ack`) is
  **not implemented server-side yet**.
- **History**: 1.1.0 shipped `API Base URL = https://workflowswift.com/api` (no `/v1`), so every
  call returned 404 while *Test Connection* still reported success. Fixed in 1.2.0, which also
  drops the `<all_urls>` host permission (it was listed next to the explicit marketplace
  origins) and moves the base URL into one shared `config.js`.

## Support

Contact David via Telegram. Tickets: `GET/POST /tickets`.
