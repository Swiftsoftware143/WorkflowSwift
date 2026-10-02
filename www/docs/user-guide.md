# User Guide — WorkflowSwift

WorkflowSwift turns a captured lead or form entry into automated, multi-step follow-up: an
incoming webhook creates an instance, the engine walks the steps, and each step can pull a
dashboard Data Card, call an endpoint you own, notify a webhook, wait, or fork.

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
  Plumbers") from the widgets in your dashboard — nothing is hardcoded. The API refuses any other
  first step, and it refuses it on **every** door that decides the order: adding a step, moving a
  step, and a **template** (which installs as a workflow — see Templates below).
- **Step types**: Data Card, AI Action, Notify, Delay/Wait, Fork, HTTP Request, Action,
  Render Video/Image/Audio, Condition, Webhook and Manual/Approval — and **every one of them
  executes**. The API refuses any other step type (`POST /workflows/{id}/steps` answers `400`), so
  a step that cannot run can no longer be created. **Research is retired** for that reason: no path
  in this app performs a research call.
- **Export is retired** (2026-10-02): the destinations it advertised — CSV download, Resend,
  SendGrid, CoreSwift CRM — have no sender and no store on either side (this app holds no
  Resend/SendGrid credential, offers no file download, and CoreSwift is an inbound lead push, not an
  export contract). Every run used to be POSTed at a platform webhook that is not registered and the
  step was **recorded as completed having exported nothing**. The API refuses the type now, the
  Builder no longer offers it, and an old Export step is skipped with a warning that says so. To get
  data out, use an **HTTP Request** step pointing at an endpoint you own.
- **Custom-code steps are retired** (2026-10-02): the **`transform`** and **`code`** step types
  accepted a block of JavaScript, and the `format` step described a transformation, but the only
  real behaviour any of them had was a JavaScript node inside n8n carrying your code verbatim. That
  node **cannot run on this install** (n8n here has no JavaScript task runner configured, and its
  own Code node fails), and turning the runner on would mean running your code inside the service
  that holds n8n's own credentials. **Nothing here executes tenant code**, so the API refuses all
  three types, the Builder never offered them, and an old step of one of those types is skipped with
  a warning that names the gap — it is never reported as having transformed anything. If you need
  custom logic, use an **HTTP Request** step to call a service you own that runs your code.
- **AI Action runs on your own provider key** (2026-10-02). In the Builder, pick AI Action, choose
  the provider — **OpenAI, Anthropic, DeepSeek or Gemini** — and write the prompt; connect that
  provider's key once under **Provider Keys**. The step sends your prompt straight to that provider
  and the run stores the provider's own reply as the step's output, together with the provider's HTTP
  status. It costs **0 credits**: it runs on your key, never a platform one. The write path refuses a
  step whose provider is not one of those four, and refuses one that names none. With no key
  connected the step is **skipped** with that reason in the run history; a provider error (a rejected
  key, a rate limit) **fails** the step and shows the provider's status code. Nothing is ever
  reported as generated unless the provider generated it.
- **Notify**: the one channel is **Webhook** — the step POSTs `{message, data}` to the URL you put in
  its Recipient field, so the receiving end gets the run's own item. The **Email and SMS channels are
  retired** (2026-10-02): this app has no tenant-triggered mail sender (its mail provider is
  template-based platform mail, not a workflow sender) and no SMS provider at all, so both were
  channels a step could be built on that delivered nothing. The API refuses them the same way it
  refuses an unknown step type, and a step stored with one keeps its place as a no-op that names the
  retirement. Its URL goes through the same destination check as an HTTP Request step, and a
  destination that answers non-2xx (or cannot be reached) fails the step — the run history records
  the status and the reason, so a notification that did not go out is never reported as sent.
- **HTTP Request / Action / Render Video/Image/Audio**: an HTTP Request or Action step calls the
  URL you configure, with the method you pick, and the run history records the status and the reply.
  A Render step calls your provider's `endpoint` and logs the result under **Renditions** (provider,
  asset id and URL come from the provider's own response). Both refuse a destination that resolves
  inside the platform's own network — loopback, private or link-local addresses are never called —
  so a step can not be used to read an internal service.
- **Deploy to n8n** (plan-gated) and **run manually**. A manual run creates an **instance** with
  traceable state, a step-by-step history and full execution logs.
- The n8n copy is what an **external** caller triggers: **`POST`** the webhook path the deploy
  response returns (`webhook_path`, and `webhook_method` names the verb — `POST`; the same path
  answers a `GET` with a 404 that names POST). n8n answers `200 "Workflow was started"` as soon as
  it accepts the trigger, so that caller never waits for the run. A run that then **fails reports
  itself**: it appears in **Instances** as `failed`, and
  its run history names the failing step and the reason the step gave. A run that **succeeds** is
  visible in n8n's own execution record and does not create an instance of its own.
  The n8n copy goes through the **same destination check** as the in-process run: a step whose URL
  resolves inside the platform's own network (loopback, private or link-local), is empty, or is an
  n8n run-time expression is **not** deployed — the deploy (or the run that mirrors it) reports the
  step and the reason and writes nothing to n8n, so n8n never calls a destination the app itself
  would refuse.
- **Credits**: charged per execution, and the amount comes from the workflow's tier
  (`simple` = 2, `medium` = 3, `complex` = 5, `ai_enhanced` = 7 — the full table is in the
  `deduct` response). Running out is refused by the API, not just warned about in the UI.
  Check `GET /credits/balance`.

## Templates

- A template is a **starting point for a workflow**: `Install as Workflow` (`POST
  /templates/{id}/install`) copies its steps into a new workflow of yours, which you can then edit
  in the Builder. `Export JSON` downloads the template as a file, `Import JSON` creates a new
  private template from one.
- Every template step is a **step type this app can actually run**, and install copies it
  as-is. That is now enforced on the way in: a template carrying a step type with no executor is
  refused — on create, on import and on install — with the offending step named and the valid
  types listed, so an install can no longer produce a workflow made of steps that do nothing.
- A template's **step 1 is the installed workflow's step 1**, so the Data-Card rule holds here too:
  a template whose first step (the lowest `sort_order`) is not a **Data Card** is refused — on
  create, on import **and** on install — with the offending step and its type named, because the
  Builder would refuse to build that workflow. (Workflows this platform created itself with another
  first step — the inbound-capture `integration` — keep running and stay editable; the rule governs
  what the API lets you *build*.)
- The gallery's **Government Contracting Lifecycle** template is a 10-stage checklist: its first
  step is the Data Card every workflow opens with (pick your dashboard widget for it), and each
  stage after it is a **Manual / Approval** step — the run parks at the stage and you **Approve**
  or **Reject** it from the instance's run history to move on.
- A template that is **not included in your plan is locked** — the lock is enforced server-side,
  so the padlock badge is not just decoration.

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
- **Integration targets** (`/integration-targets`) are the configured destinations a run can push
  to (webhook URL, provider preset, daily cap). They are set up for your account by the operator, and
  so is a step's binding to one: the **Builder has no target picker and the steps API accepts no
  binding field**, so a workflow you build in the app does not dispatch to a target. What you
  configure yourself is the **Integration Center** above.
- **Incoming webhook**: `POST /api/v1/incoming` is what other Swift tools push leads to. It is
  protected by an internal key.

## Chrome extension — Swift Market Intel

- **Download**: `https://workflowswift.com/swift-market-intel-extension-1.2.0.zip`
  (rolling alias `/swift-market-intel-extension.zip`; pin the versioned file — the alias can lag
  behind the CDN cache). Install guide: `/swift-market-intel-extension.html`.
- **Install**: unpack the zip, then `chrome://extensions` → Developer mode → *Load unpacked* →
  pick the folder containing `manifest.json`. Not on the Chrome Web Store, so updates are manual.
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
