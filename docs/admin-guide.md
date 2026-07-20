# Admin Guide — WorkflowSwift

## Admin Endpoints

All admin endpoints require a super admin JWT token (David's account).

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/admin/usage` | GET | Usage dashboard — credits, workflow runs, n8n status per account |
| `/api/v1/admin/accounts` | GET | List all accounts with retention + n8n_provisioned flag |
| `/api/v1/admin/accounts/{id}` | DELETE | Permanently delete account + all data |
| `/api/v1/admin/accounts/{id}/retention` | PUT | Override account retention policy |
| `/api/v1/admin/accounts/create` | POST | Create new account |
| `/api/v1/admin/plans` | GET/POST | List/create plan tiers |
| `/api/v1/admin/settings` | GET | List all admin settings |
| `/api/v1/admin/settings/{key}` | GET/PUT | Get/update specific setting |
| `/api/v1/bridge/tasks` | GET | List AI bridge inbound files |
| `/api/v1/bridge/results` | GET | List AI bridge outbound results |

## Workspaces & Multi-Industry

Users create **workspaces** (portfolio companies) from their dashboard. Each workspace can have its own **industry** which determines dashboard layout and workflow templates.

**Registration:** New users choose an industry at signup. Their dashboard auto-seeds with industry-specific widgets.

**Multi-industry:** Higher-tier accounts can add multiple industries via `POST /api/v1/accounts/add-industry`. Each gets its own dashboard with widgets.

### Workspace Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/workspaces` | GET | List user's workspaces |
| `/api/v1/workspaces` | POST | Create workspace (accepts `industry_slug`) |
| `/api/v1/workspaces/{id}` | DELETE | Delete workspace |
| `/api/v1/workspaces/{id}/stats` | GET | Workspace-scoped counts |

### Paperclip Dashboard Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/dashboard/workspace` | GET | Active instances + automation stats (opt. `?workspace_id=`) |
| `/api/v1/dashboard/timeline` | GET | Activity timeline (opt. `?workspace_id=&days=`) |
| `/api/v1/dashboard/stats` | GET | Aggregate counts |
| `/api/v1/dashboard/widgets` | GET | Industry-specific widgets (`?industry=`) |
| `/api/v1/dashboard/push-widget-data` | POST | Push metric data to widget |
| `/api/v1/dashboard/activity` | GET | Recent activity feed |

#
## Industry Data Sources (Satellites)

Admin-configurable data sources that power industry-specific dashboard widgets. Each source costs credits per API call.

### Admin Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/admin/industry-sources` | GET | List all sources (opt. `?industry=`) |
| `/api/v1/admin/industry-sources` | POST | Create/update a data source |
| `/api/v1/admin/industry-sources/seed` | POST | Seed defaults (5 sources per industry) |
| `/api/v1/admin/industry-sources/{id}` | DELETE | Remove a data source |

### Tabbed Dashboard

The dashboard now supports industry-specific tabs. Each workspace shows a **General** tab plus one tab per industry the account has selected.

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/dashboard/industry-tabs` | GET | Tab-navigated dashboard (opt. `?workspace_id=`) |

### Source Types

- **api** — External API integration (credit cost per call)
- **webhook** — Push-based data ingestion
- **rss** — Feed-based updates (hourly refresh)
- **scraper** — Scheduled data scraping

### Default Sources (Seeded per Industry)

All 19 industries get 5 default sources: market_research (2 credits), news_feed (1), competitor_intel (3), lead_finder (5), trend_analytics (2).

## Industry Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/industries` | GET | List all industries (public) |
| `/api/v1/accounts/industry` | GET | Get account's industries |
| `/api/v1/accounts/add-industry` | POST | Add industry (creates dashboard) |

## Agents & Kanban

Paperclip agents can be created per workspace. Each agent has a role, budget, and credit tracking. Tickets act as a kanban board for tracking work items.

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/agents` | POST | Create agent (accepts `name`, `role`, `workspace_id`) |
| `/api/v1/agents` | GET | List agents (opt. `?workspace_id=`) |
| `/api/v1/workspaces/{id}/tickets` | GET | List workspace tickets |
| `/api/v1/workspaces/{id}/tickets/{tid}/status` | PATCH | Update ticket status |

## BYOK Integrations (Provider Keys)

Per-workspace API key management for external providers. Keys are scoped to the workspace — not shared globally. **All keys are encrypted at rest** using pgcrypto (PGP symmetric encryption) with a per-account encryption key.

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/workspaces/{id}/provider-keys` | GET | List configured providers |
| `/api/v1/workspaces/{id}/provider-keys` | POST | Save/update provider key |
| `/api/v1/workspaces/{id}/provider-keys/{provider}` | DELETE | Remove provider key |


## Rate Limiting

All protected endpoints are rate-limited per account: **30 requests/second**, burst of 10. Returns HTTP 429 with `Retry-After` header when exceeded.

## Throttles

- **n8n workers**: 2 workers, concurrency=10 each (20 concurrent max)
- **Worker 3**: Available in compose scale profile — activate when approaching 500 users
- **Credits**: Each workflow execution costs 1 credit. Users without credits cannot run workflows.
