# WorkflowSwift — Visual Step Builder Admin Guide

## Overview
The Visual Step Builder is a full CRUD system for managing workflow execution steps. This guide covers the backend API, database schema, deployment, and troubleshooting.

## Architecture

```
Frontend (index.html) ←→ API Layer (Rust/Axum) ←→ PostgreSQL
                              │
                     Workflow Execution Engine
```

Steps are stored as ordered entities within a workflow. Each step has a `sort_order` field that determines execution sequence.

### Troubleshooting

**Issue: Workflow execution engine is stuck**
- Check PostgreSQL connection pool — exhausted connections block all engine operations
- Verify the executor worker count: `journalctl -u workflowswift-api.service | grep executor`
- Look for deadlocked step execution in logs: `journalctl -u workflowswift-api.service | grep -i deadlock`
- Restart the service if a step has been in "running" state for >10 minutes

**Issue: API latency spikes under load**
- Check if the `sort_order` index has sufficient memory: `EXPLAIN ANALYZE SELECT * FROM workflow_steps WHERE workflow_id = '<uuid>' ORDER BY sort_order;`
- Monitor Postgres query performance via `pg_stat_activity` for long-running queries
- Consider adding a composite index on `(workflow_id, sort_order)` if not present
- Verify connection pool size in the service config — default is 20, bump to 50 for higher traffic

---

## API Reference

### Base URL
```
https://workflowswift.com/api/v1/workflows
```

### Headers
```
Authorization: Bearer <tenant_token>
Content-Type: application/json
```

### Endpoints

#### List Steps
```
GET /api/v1/workflows/{id}/steps
```
Returns all steps for a workflow ordered by `sort_order`.

**Response:**
```json
{
  "steps": [
    {
      "id": "uuid",
      "workflow_id": "uuid",
      "step_type": "http_request",
      "name": "Fetch Data",
      "description": "Get user profile",
      "sort_order": 0,
      "config": { "url": "...", "method": "GET" },
      "created_at": "2026-07-03T00:00:00Z",
      "updated_at": "2026-07-03T00:00:00Z"
    }
  ]
}
```

#### Create Step
```
POST /api/v1/workflows/{id}/steps
```
**Body:**
```json
{
  "step_type": "http_request",
  "name": "Fetch Data",
  "description": "Get user profile",
  "config": { "url": "https://api.example.com", "method": "GET" }
}
```
`sort_order` is auto-assigned (max existing + 1). Returns the created step.

#### Update Step
```
PUT /api/v1/workflows/{id}/steps/{step_id}
```
**Body:**
```json
{
  "step_type": "http_request",
  "name": "Updated Name",
  "description": "Updated description",
  "sort_order": 2,
  "config": { "url": "https://api.example.com/v2", "method": "POST" }
}
```
All fields optional. Omitting `sort_order` leaves the current order unchanged.

#### Delete Step
```
DELETE /api/v1/workflows/{id}/steps/{step_id}
```
Deletes the step. Remaining steps keep their current `sort_order` values.

#### Reorder Steps
```
PUT /api/v1/workflows/{id}/steps/reorder
```
**Body:**
```json
{
  "step_ids": ["uuid-1", "uuid-2", "uuid-3"]
}
```
Reassigns `sort_order` (0, 1, 2, ...) based on array position.

## Database Schema

### Troubleshooting

**Issue: Migrations fail to apply**
- Check that the migration file has a unique timestamp prefix — duplicates cause silent skips
- Verify the `_migrations` table exists and is not locked: `SELECT * FROM _migrations ORDER BY applied_at DESC LIMIT 5;`
- Run `cargo run --bin migrate -- redo <migration_id>` to retry a failed migration
- Ensure the Postgres user has `CREATE` and `ALTER` privileges on the `workflowswift` database

**Issue: Query performance degrading over time**
- Check for missing indexes on frequently filtered columns (`tenant_id`, `workflow_id`, `step_type`)
- Run `VACUUM ANALYZE workflow_steps;` to update table statistics
- Monitor table bloat: `SELECT schemaname, tablename, n_dead_tup FROM pg_stat_user_tables WHERE n_dead_tup > 1000;`
- Schedule regular `VACUUM` via a cron or pg_cron job

**Issue: Foreign key constraint errors in production**
- Verify that `workflows(id)` exists before inserting steps — orphaned steps indicate a bug in the creation flow
- Check if cascading deletes are firing correctly: `ON DELETE CASCADE` only works if the FK constraint is present
- Review the `updated_at` trigger — missing trigger causes stale timestamps on updates

```sql
CREATE TABLE workflow_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_id UUID NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    step_type VARCHAR(50) NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    config JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_workflow_steps_workflow_id ON workflow_steps(workflow_id);
CREATE INDEX idx_workflow_steps_tenant_id ON workflow_steps(tenant_id);
```

## Deployment

### Build
```bash
cd /opt/swift/workflowswift
cargo build --release
```

### Service
The API runs as a systemd service:
```
workflowswift-api.service
```

### Restart
```bash
systemctl restart workflowswift-api.service
```

### Troubleshooting

**Issue: Service won't start after deployment**
- Check the binary path in the systemd unit file: `systemctl cat workflowswift-api.service`
- Verify the binary has execute permissions: `ls -la /opt/swift/workflowswift/target/release/workflowswift-api`
- Look for missing `.env` or config files: `cat /opt/swift/workflowswift/.env`
- Check if the port is already in use: `ss -tlnp | grep <port>`

**Issue: Build fails with dependency errors**
- Ensure Rust toolchain is up to date: `rustup update stable`
- Clear build cache and retry: `cargo clean && cargo build --release`
- Check for broken crate versions in `Cargo.lock` — try `cargo update` if a specific crate fails
- Verify disk space: `df -h /opt` — a full disk causes linker errors

**Issue: Database connection errors after deploy**
- Verify the `DATABASE_URL` in the service environment matches the running Postgres instance
- Check Postgres is running: `systemctl status postgresql` or `docker ps | grep postgres`
- Confirm the Postgres user has the correct password and the database exists: `psql -U workflowswift -d workflowswift -c "SELECT 1"`
- Check firewall rules on port 5432 if connecting remotely

**Issue: Rolling back a bad deployment**
- Keep the previous binary: `/opt/swift/workflowswift/target/release/workflowswift-api.bak`
- Swap it back: `cp /opt/swift/workflowswift/target/release/workflowswift-api.bak /opt/swift/workflowswift/target/release/workflowswift-api && systemctl restart workflowswift-api.service`
- If using a symlink, just repoint: `ln -sf /opt/swift/workflowswift/releases/v1.2.3/workflowswift-api /opt/swift/workflowswift/workflowswift-api && systemctl restart workflowswift-api.service`

---

## Step Types Reference

Valid `step_type` values:
| Type | Enum Value | Config Schema |
|------|-----------|---------------|
| Data Card | `data_card` | `{ title, content }` |
| HTTP Request | `http_request` | `{ url, method, headers?, body? }` |
| Delay | `delay` | `{ duration_seconds }` |
| Condition | `condition` | `{ variable, operator, value }` |
| Integration | `integration` | `{ integration_type, action, ... }` |
| Email | `email` | `{ to, subject, body }` |
| Notification | `notification` | `{ channel, title, message }` |
| Webhook | `webhook` | `{ url, method, payload }` |
| Playwright | `playwright` | `{ url, action, selector? }` |
| AI Prompt | `ai_prompt` | `{ prompt, model, variables? }` |

### Troubleshooting

**Issue: Invalid step type errors when creating steps**
- Verify the `step_type` matches one of the valid enum values exactly (case-sensitive)
- Check that the config JSON matches the schema for that type — missing required fields cause validation rejection
- Look at API logs for the full validation error: `journalctl -u workflowswift-api.service | grep validation`
- If a custom step type was added in a migration, ensure the Rust enum is also updated and the binary rebuilt

**Issue: Integration step type not resolving**
- Confirm the user has the required integration configured in their Integration Center
- Check that the `integration_type` in the config matches an active, healthy connection
- Verify the destination selector is populated — empty destination lists usually mean the external API returned an error

---

## Troubleshooting

### Steps not saving
1. Check that the workflow exists and belongs to the current tenant
2. Verify the JWT token is valid and not expired
3. Check API logs: `journalctl -u workflowswift-api.service -n 50`

### Reorder not working
- Ensure all step IDs in the request belong to the same workflow
- Verify they match exactly (UUIDs are case-sensitive)

### Step not appearing in pipeline
- Hard refresh the page (Ctrl+Shift+R)
- Check browser console for API errors
- Verify the workflow has steps: `GET /api/v1/workflows/{id}/steps`

### API returns 500
- Check Postgres connection: `docker exec -it workflowswift-postgres psql -U workflowswift -c "SELECT 1"`
- Check disk space: `df -h`
- Review logs: `journalctl -u workflowswift-api.service -n 100`

## Testing

### Manual API Test Sequence
```bash
# 1. Create step
curl -X POST /api/v1/workflows/{id}/steps \
  -H "Content-Type: application/json" \
  -d '{"step_type":"http_request","name":"Test","config":{"url":"https://example.com","method":"GET"}}'

# 2. Update step
curl -X PUT /api/v1/workflows/{id}/steps/{step_id} \
  -H "Content-Type: application/json" \
  -d '{"name":"Updated","sort_order":1}'

# 3. Reorder
curl -X PUT /api/v1/workflows/{id}/steps/reorder \
  -H "Content-Type: application/json" \
  -d '{"step_ids":["id-2","id-1","id-3"]}'

# 4. Delete
curl -X DELETE /api/v1/workflows/{id}/steps/{step_id}

# 5. List (verify)
curl /api/v1/workflows/{id}/steps
```

### Troubleshooting

**Issue: API test returns 401 Unauthorized**
- The token is expired or invalid — regenerate via the API Keys page and update your test scripts
- Verify the token prefix matches what's displayed in the UI (first 8 chars)
- Check that the token hasn't been revoked via the Integrations page

**Issue: Reorder endpoint returns 400 Bad Request**
- Confirm all UUIDs in the `step_ids` array belong to the same workflow
- Check for duplicate UUIDs in the array — the endpoint rejects duplicates
- Ensure the array is not empty — at least one step UUID is required

**Issue: Delete returns 404**
- The step UUID may be from a different workflow or tenant — verify the workflow ID and step ID match
- The step may already have been deleted — check by listing steps for the workflow
- Check API logs for the full request path: `journalctl -u workflowswift-api.service | grep DELETE`

---

## Integration Center — User Connectivity

Every user account gets an **Integration Center**. This is where they manage all external connectivity — both native SwiftSoftware tools and third-party services.

### Where users find it
- **User menu** (top-right avatar dropdown) → **API Keys** — auto-generated keys for other tools to connect *to* WorkflowSwift
- **User menu** → **Integrations** — the Integration Center where they connect WorkflowSwift *to* other tools
- Both are also accessible from **Settings**

### Auto-generated keys (User menu → API Keys)
Every account gets these automatically at signup. These keys allow external tools to authenticate with WorkflowSwift on behalf of this user:

| Key | Purpose |
|-----|---------|
| **Primary API Key** | Main auth token for any external caller hitting the WorkflowSwift API. Used when Mailchimp, Zapier, or custom scripts trigger workflows or push data. |
| **Webhook Secret** | HMAC secret for verifying incoming webhook payloads. External services sign requests with this so the system knows they're legit. |
| **Surface Token** | Per-surface auth token for surface-specific integrations (e.g., a CRM surface calling back into workflow triggers). |

Users can copy, regenerate (old key immediately invalidated), or revoke any key.

### Native SwiftSoftware integrations (built-in, no setup)
All SwiftSoftware tools are pre-connected. If the user has an account, they see it as an available integration — no API keys to paste:

| Product | What connects |
|---------|--------------|
| **CoreSwift (CRM)** | Contact sync, lead triggers, deal updates, activity logging |
| **FunnelSwift** | Landing page form submissions → workflow triggers, data → CRM |
| **IncentiveSwift** | Reward triggers, loyalty point actions, referral tracking |

Each native integration shows as a toggle in the Integration Center. User can enable/disable without losing credentials. If they don't have an account yet, it shows a **Get Started** link instead.

### Third-party integrations (bring your own key)
Users connect external services by pasting their own credentials:

| Service | What they configure | Used By |
|---------|-------------------|---------|
| **OpenAI** | API key | AI Prompt steps |
| **Anthropic** | API key | AI Prompt steps |
| **OpenClaw (BYOK)** | Gateway URL + auth token | Lets users route workflows through their own OpenClaw to drive the entire system |
| **n8n** | Self-hosted URL + API key | Custom automation, advanced orchestration |
| **Mailchimp** | API key + audience ID | Email/audience steps |
| **ActiveCampaign** | API key + account URL | Auto-responder steps |
| **ConvertKit** | API key | Auto-responder steps |
| **HubSpot** | OAuth or private app token | CRM steps |
| **Salesforce** | OAuth or API key | CRM steps |
| **SendGrid** | API key | Email steps |
| **SMTP (custom)** | Server, port, credentials | Email steps |
| **Browserbase / Playwright** | Endpoint URL | Playwright steps |

### Integration step levels — cascading destination selectors

The **Integration** step type uses a 3-level cascade — like Zapier or Make — where each product defines its own destination types. After the user picks a product and action, the system fetches their actual data for the final dropdown.

**Example flow: Mailchimp → Add Subscriber**
```
Level 1: Product     → Mailchimp
Level 2: Action      → Add Subscriber
Level 3: Destination → [user's actual Mailchimp audiences fetched via API]
                      └─ Monthly Newsletter
                      └─ Promo List
                      └─ VIP Customers
```

**Destination types per product:**

| Product | Available Destinations |
|---------|----------------------|
| CoreSwift | List, Pipeline Stage, Tags |
| FunnelSwift | Landing Page, Tags |
| IncentiveSwift | Campaign, Milestone Level |
| Mailchimp | Audience, Automation Email |
| ActiveCampaign | List, Automation, Tags |
| ConvertKit | Form, Tag, Sequence |
| HubSpot | List, Pipeline, Sequence |
| Salesforce | Campaign, Report |
| SendGrid | List, Segment |
| Slack | Channel |
| Discord | Channel |
| Google Sheets | Spreadsheet, Sheet Tab |
| Stripe | Product, Price ID |

Destinations are fetched live from the connected service when the user opens the step config dropdown. The system caches results briefly but refreshes on page load.

### How workflow steps resolve connections at runtime
Every step pulls its credentials from the user's Integration Center — the user sets them once and every workflow step inherits them:

| Step Type | Resolves From | Type |
|-----------|--------------|------|
| AI Prompt | OpenAI / Anthropic key from Integrations | BYOK (or platform fallback) |
| HTTP Request | User's primary API key for SwiftSoftware-to-SwiftSoftware calls; user-configured auth for external | Both |
| Integration | CRM (CoreSwift/HubSpot/Salesforce), auto-responder (ActiveCampaign/Mailchimp), or platform connector | Native first, BYOK fallback |
| Email | SMTP / SendGrid config from Integrations | BYOK |
| Notification | Surface token from API Keys | Auto-generated |
| Webhook | Webhook secret from API Keys | Auto-generated |
| Playwright | Browserbase / Playwright endpoint from Integrations | BYOK |
| Data Card | None (static display) | — |
| Delay | None (timer) | — |
| Condition | None (logic) | — |

### Connection health
The Integration Center shows live status for every connection:
- ✅ **Connected** — last health check passed
- ⚠️ **Error** — last check failed (shows error message)
- ⚪ **Disabled** — user toggled it off
- 🔵 **Pending** — credentials saved but not tested yet

A **Test Connection** button lets users validate credentials before saving.

### API for the integration system
```
GET  /api/v1/integrations                — List all integrations (native + BYOK)
POST /api/v1/integrations                — Add/update a BYOK integration
DELETE /api/v1/integrations/{id}         — Remove an integration
GET  /api/v1/integrations/native         — List available native SwiftSoftware integrations
POST /api/v1/integrations/native/{id}/toggle — Enable/disable native integration
GET  /api/v1/integrations/keys           — List auto-generated API keys (masked)
POST /api/v1/integrations/regenerate     — Regenerate primary API key
GET  /api/v1/integrations/status         — Connection health for all active integrations
```

### Database
```sql
-- Auto-generated keys for external tools to talk to WorkflowSwift
CREATE TABLE user_api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    key_type VARCHAR(50) NOT NULL,  -- 'primary', 'webhook_secret', 'surface_token'
    key_hash VARCHAR(255) NOT NULL,
    key_prefix VARCHAR(8) NOT NULL,  -- first 8 chars for display
    label VARCHAR(255),
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Both native SwiftSoftware integrations and BYOK third-party connections
CREATE TABLE user_integrations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    provider VARCHAR(100) NOT NULL,  -- 'coreswift', 'funnelswift', 'openai', 'hubspot', etc.
    integration_type VARCHAR(20) NOT NULL DEFAULT 'byok',  -- 'native' or 'byok'
    config_encrypted JSONB NOT NULL DEFAULT '{}',
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    last_health_status VARCHAR(20),  -- 'connected', 'error', 'pending', null
    last_health_check_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_user_integrations_user ON user_integrations(user_id);
CREATE INDEX idx_user_integrations_type ON user_integrations(integration_type);
```

### Troubleshooting

**Issue: Integration shows "Error" status**
- Check the error message in the Integration Center — common failures include expired API keys, revoked OAuth tokens, or rate limits
- For BYOK integrations, ask the user to regenerate the key on the provider side, then update it in WorkflowSwift
- For native integrations, verify the user's account on that SwiftSoftware product is still active
- Run the health check test manually via `POST /api/v1/integrations/{id}/test`

**Issue: Third-party integration stops working after a deployment**
- The encrypted credentials may have been rotated — check if the encryption key changed via config
- Verify the `config_encrypted` field is decryptable: check service logs for decryption errors
- If the encryption key changed, run the re-encryption migration: `cargo run --bin migrate -- reencrypt-integrations`

**Issue: API key regeneration doesn't take effect**
- Cached JWTs may still reference the old key — wait for the token expiry or ask the user to re-authenticate
- Verify the `last_used_at` field updates when the new key is used — if not, the API auth middleware may need a restart
- Check that the `key_hash` was updated in the database: `SELECT key_prefix, LEFT(key_hash, 16) FROM user_api_keys WHERE user_id = '<uuid>';`

**Issue: User sees "Integration not available" for a native product**
- The user may not have an account on that SwiftSoftware product — they'll see "Get Started" instead of a toggle
- The admin may have disabled cross-product sharing in tenant settings — check `tenant_settings.enable_native_integrations`
- Verify the SSO/SAML identity provider is passing the correct email between products

**Issue: Destination selectors are empty (Level 3 cascade)**
- The external API may be down or rate-limiting — check the provider's status page
- The cached destinations may be stale — hard refresh or wait for the cache expiry (typically 5 minutes)
- Check the provider connection health first — an error connection returns empty destinations
- Test the destination API call directly: `curl -H "Authorization: Bearer $KEY" "https://<provider-api>/audiences"`

**Issue: Integration config data isn't saved (BYOK keys vanish on page reload)**
- The `config_encrypted` field may be failing to serialize — check that all nested JSON is valid
- The encryption function may be throwing an error — look for `crypto` or `encrypt` entries in the service logs
- If the database write succeeds but the read returns empty, check for a migration that dropped the column

---

## Surface Filter — Workflows & Templates

The **Workflows** and **Templates** pages include a **Surface** dropdown filter next to the search bar. This lets you narrow the list by surface assignment.

### How it works
- The dropdown fetches available surfaces from `GET /api/v1/surfaces`
- Options include **All Surfaces** (default), **No Surface** (unassigned items), and each named surface
- Selection applies client-side, filtering by `surface_id` on both workflows and templates
- Works alongside the existing search bar and Industry filter on Templates

### Backend API
Workflows and Templates endpoints accept an optional `?surface=<uuid>` query parameter:
```
GET /api/v1/workflows?surface=<uuid>
GET /api/v1/workflow-templates?surface=<uuid>
GET /api/v1/workflows
GET /api/v1/workflow-templates
```
Omitting the parameter returns all items regardless of surface.

### Database
- `workflows.surface_id` — nullable UUID referencing `surfaces(id)`
- `workflow_templates.surface_id` — nullable UUID referencing `surfaces(id)`

### Troubleshooting

**Issue: Surface filter shows no results even though workflows exist**
- Verify the workflow or template has a `surface_id` set — null/blank surfaces only show under "No Surface" option
- Check that the surface UUID matches exactly — a leading/trailing space in the filter dropdown causes empty results
- Run the backend query directly: `SELECT id, name FROM workflows WHERE surface_id IS NOT NULL LIMIT 5;`

**Issue: Surface dropdown doesn't load any options**
- Check that the `GET /api/v1/surfaces` endpoint is returning data: `curl -H "Authorization: Bearer <token>" https://workflowswift.com/api/v1/surfaces`
- Verify that surfaces are created in the database: `SELECT id, name FROM surfaces LIMIT 10;`
- If the endpoint returns 500, check Postgres connection and `surfaces` table permissions

**Issue: Filter by surface query parameter ignored**
- Confirm the query parameter is being sent as `surface` (not `surface_id`) — the API expects the exact param name
- Check that the backend handler actually reads `surface` from query params — a stale build may not include the filter logic
- Test with a direct curl call to isolate frontend vs backend issue

---

## Settings — Industry Dashboard Assignment

The **Settings** page (⚙️ in sidebar) lets you choose which industry dashboard and template library your account sees.

### How to use
1. Click **Settings** in the left sidebar
2. You'll see your current industry displayed
3. Open the **Industry Dashboard** dropdown — shows all 18 available industries with template counts
4. Select the industry you want (e.g. Content Creation, Newsletter, GovCon, etc.)
5. Click **Save**

**What changes:**
- Dashboard widgets reload for the new industry
- Template gallery filters to show only relevant templates
- Sidebar nav stays the same — only the content adapts

### Troubleshooting

**Issue: Industry selection doesn't persist after page reload**
- Check that the `POST /api/v1/settings/industry` request succeeds (look in browser Network tab)
- Verify the setting is stored in the database: `SELECT value FROM user_settings WHERE user_id = '<uuid>' AND key = 'industry_id';`
- If stored but not loading, check the settings retrieval endpoint — it may have a caching issue
- Clear browser cache and re-save — stale local storage can override server state

**Issue: User sees wrong dashboard widgets after switching industries**
- The dashboard widgets are fetched from the backend — check that the new industry has widgets associated
- Verify in the database: `SELECT industry_id, widget_type FROM dashboard_widgets WHERE industry_id = '<new_id>';`
- If the industry has no widgets, the dashboard will show empty — assign widgets in the admin panel or via migration

**Issue: Incorrect number of templates displayed after industry change**
- The template count shown in the dropdown may be stale (cached) — hard refresh the page
- Verify the actual count: `SELECT COUNT(*) FROM workflow_templates WHERE industry_id = '<uuid>';`
- Check that the template filter logic respects the `industry_id` — a missing `WHERE` clause shows all templates

---

## Gallery — Asset Mirror

The **Gallery** (🖼️ in sidebar) displays a read-only feed of media assets created by your Content Creation workflows. Assets stay in their original software (VideoExpress, Artistly, CloneVoice) — the gallery mirrors them with deep-links.

### How to use
1. Click **Gallery** in the left sidebar
2. View all created assets in a grid
   - 🎬 **VideoExpress** — Talking photos, videos
   - 🎨 **Artistly** — AI-generated images/art
   - 🎙️ **CloneVoice** — Voice clones
3. Filter by source using the dropdown
4. Search by asset name
5. Click **🔗 Open in [source]** to jump directly to the asset in the external tool

### How assets get here
When an n8n Content Creation workflow runs, it pushes results to:
```
POST /api/v1/dashboard/push-widget-data
{
  "metric_key": "content_creation_gallery",
  "value": {
    "items": [{
      "name": "Summer Promo Video",
      "source": "videoexpress",
      "external_url": "https://videoexpress.ai/project/abc123",
      "status": "completed",
      "created_at": "2026-07-04T21:00:00Z"
    }]
  }
}
```
Each workflow run adds items. You can remove individual items or clear all.

### Troubleshooting

**Issue: Assets not appearing in Gallery after workflow run**
- Check that the workflow completed successfully — a failed n8n run won't push assets
- Verify the push endpoint returned 200: `POST /api/v1/dashboard/push-widget-data` — check workflow execution logs
- Confirm the `metric_key` is exactly `content_creation_gallery` — a typo causes silent discard
- Check the database: `SELECT * FROM dashboard_widget_data WHERE metric_key = 'content_creation_gallery' AND user_id = '<uuid>' ORDER BY created_at DESC LIMIT 5;`

**Issue: "Open in [source]" link is broken**
- The `external_url` in the push payload may be incorrect or expired — check the workflow step generating it
- For VideoExpress/Artistly/CloneVoice, verify the user's session is still valid in that external tool
- Some external tools use expiring URLs — the link may work only for a limited window after creation

**Issue: Asset appears in multiple users' galleries**
- The push may have been made with a tenant-level API key instead of a user-specific token
- Verify that the `user_id` being set in the push handler matches the intended recipient
- Check the dashboard widget data table: `SELECT user_id, COUNT(*) FROM dashboard_widget_data WHERE metric_key = 'content_creation_gallery' GROUP BY user_id;`

**Issue: Cannot remove individual items from Gallery**
- Confirm the delete endpoint is being called with the correct item ID
- Check that the user owns the asset — deletion is scoped to the authenticated user's tenant
- Verify cascade delete constraints — deleting the parent workflow run should also clean up gallery items

---

## Content Creation Workflow

**Template:** AI Content Creation - VideoExpress + Artistly + CloneVoice
**Category:** Content Creation

### Modes
| Mode | What it does |
|------|-------------|
| `videoexpress` | Generate talking photos / videos only |
| `clonevoice` | Generate voice clones only |
| `artistly` | Generate AI images/art only |
| `full_pipeline` | Run all three in sequence |

- 1 credit per execution regardless of mode
- Results auto-push to the Gallery and Dashboard

### Troubleshooting

**Issue: Content Creation workflow fails on first run**
- Verify the user's Integration Center has the required API keys (OpenAI, Browserbase, etc.) — the step will fail if credentials are missing
- Check that the `mode` parameter matches a valid mode exactly — invalid mode causes an immediate rejection
- Look for rate limit errors in the logs: `journalctl -u workflowswift-api.service | grep -i rate`
- Test with the simplest mode (`videoexpress`) first before running `full_pipeline`

**Issue: Credits not deducted after successful run**
- Check the credit deduction handler in the workflow execution engine — a bug in the credit middleware lets runs through without decrementing
- Verify the user's credit balance: `SELECT balance FROM credits WHERE user_id = '<uuid>';`
- If credits are not deducted on failure, check that the failure path also calls the credit rollback

**Issue: Pipeline step hangs at AI generation**
- The external AI API may be timing out — increase the HTTP client timeout in the service config
- Check if the prompt is too long — API providers have token limits that silently truncate or reject
- Look for out-of-memory errors on the VPS: `dmesg | grep -i oom` — AI processing is memory-intensive

**Issue: Multiple runs in sequence trigger rate limits on external APIs**
- Add a delay step between AI generation calls in the workflow template
- The external provider (OpenAI, Anthropic) enforces per-minute rate limits — stagger runs by at least 30 seconds
- Monitor rate limit headers in the service logs: `X-RateLimit-Remaining` and `Retry-After`

---

## Newsletter Templates

**Category:** Newsletter — Email newsletter creation, automation, and management.

### Available templates
- **Newsletter Auto-Pilot** — Automated email newsletter creation: content curation, template assembly, scheduling, delivery tracking
- **Newsletter Campaign** — Campaign-style newsletter with audience segmentation and analytics

### How to use
1. Go to **Templates** → filter by **Newsletter** category
2. Clone a template into a new workflow
3. Wire up your email service provider (Mailchimp, SendGrid, etc.)
4. Run on schedule or trigger manually

### Troubleshooting

**Issue: Newsletter workflow fails at email delivery step**
- Verify the email provider credentials are correct in the Integration Center (SMTP, SendGrid, Mailchimp)
- Check for sending limits — most email providers have daily caps that silently drop messages
- Test the email provider connection directly: run the health check in the Integration Center
- Look for SPF/DKIM failures in the delivery logs — misconfigured DNS causes emails to be rejected by recipients

**Issue: Newsletter templates not appearing in template library**
- Confirm the industry is set to **Newsletter** in Settings — templates are filtered by industry
- Check that the templates have `industry_id` set to the Newsletter category in the database
- Verify the templates are published: `SELECT id, name, is_published FROM workflow_templates WHERE industry_id IN (SELECT id FROM industries WHERE slug = 'newsletter');`

**Issue: Auto-scheduled newsletter doesn't fire**
- Check that the workflow has an active schedule configured (CRON or interval)
- Verify the scheduler service is running: `systemctl status workflowswift-scheduler.service`
- Look for missed triggers: `journalctl -u workflowswift-scheduler.service | grep -i miss`
- Check that the workflow is in "Active" status — paused or draft workflows are skipped by the scheduler

---

## Full Industry List (18 categories)

| # | Industry | Slug | Templates |
|---|----------|------|-----------|
| 1 | Sales & Lead Generation | sales-lead-gen | 3 |
| 2 | Onboarding | onboarding | 1 |
| 3 | Service Businesses | service-businesses | 5 |
| 4 | Marketing | marketing | 2 |
| 5 | Recruitment & Staffing | recruitment-staffing | 3 |
| 6 | Marketing Agencies | marketing-agencies | 2 |
| 7 | Operations | operations | 2 |
| 8 | Professional Services | professional-services | 4 |
| 9 | Ecommerce & Retail | ecommerce-retail | 3 |
| 10 | Healthcare & Wellness | healthcare-wellness | 3 |
| 11 | Construction & Development | construction-development | 3 |
| 12 | Grant & Funding | grant-funding | 3 |
| 13 | Education & Training | education-training | 3 |
| 14 | Content Creation | content-creation | 1 |
| 15 | Publishing & Media | publishing-media | 3 |
| 16 | Site Flipping | site-flipping | 3 |
| 17 | Government Contracting | government-contracting | 9 |
| 18 | Newsletter | newsletter | 1 |

To change your industry assignment, go to **Settings** in the sidebar.

### Troubleshooting

**Issue: Industry list doesn't show all 18 categories**
- Check the `industries` table: `SELECT COUNT(*) FROM industries;` — if fewer than 18, the seed migration may not have run
- Run the industry seeder: `cargo run --bin seed -- industries`
- Verify the API endpoint returns all: `curl https://workflowswift.com/api/v1/industries | jq '.'`

**Issue: Newly added industry doesn't appear in the Settings dropdown**
- The dropdown is populated from `GET /api/v1/settings/industries` — ensure this endpoint doesn't have a hardcoded list
- Check that the industry has at least one template assigned — some dropdowns filter to industries with templates
- Restart the API service to clear any in-memory cache: `systemctl restart workflowswift-api.service`

**Issue: Template counts in industry list don't match actual counts**
- The count shown in the Full Industry List may be stale — it's updated when a new template is published
- Verify actual counts: `SELECT i.id, i.name, COUNT(wt.id) FROM industries i LEFT JOIN workflow_templates wt ON wt.industry_id = i.id GROUP BY i.id, i.name;`
- If counts are cached, run the count refresh: `cargo run --bin refresh-template-counts`

**Issue: Industry list includes industries the tenant shouldn't have access to**
- Industry visibility can be scoped by tenant — check `tenant_industry_access` if that feature is enabled
- Verify that the admin hasn't restricted industries in the tenant config: `SELECT restricted_industries FROM tenants WHERE id = '<uuid>';`
- If restrictions are in place, the API must filter before returning the list — check the backend handler

---

## Admin Features

### Role System

WorkflowSwift uses a three-tier role system:

| Role | Permission Level | Description |
|------|-----------------|-------------|
| **super_admin** | Full system access | Can manage accounts, email templates, and all tenants. Only David (swiftsoftware143@yahoo.com) has this role. |
| **user** | Account-level access | Standard user tied to an account. Cannot create new accounts or manage system-wide settings. |
| **team_member** | Scoped account access | Invited users with granular permissions within an account. Assigned via the team invite flow. |

The `perm_is_super_admin` boolean flag on the users table distinguishes super admins from other users. This flag is set at the database level and cannot be changed through the API.

New signups are automatically assigned `role: "user"` with no admin privileges.

### Creating Accounts (Super Admin Only)

**Endpoint:** `POST /api/v1/admin/accounts/create`

Only super admins can create new accounts. This creates an account, a user (role: "user"), assigns a plan, and sends a welcome email with a temporary password.

**Request body:**
```json
{
  "name": "Jane Doe",
  "email": "jane@example.com",
  "account_name": "Example Corp",
  "plan_slug": "starter",
  "industry_slug": "real-estate"
}
```

**Validation rules:**
- `plan_slug` must match an existing plan in the `plans` table
- `industry_slug` must match an existing industry in the `industries` table
- Email must not already be registered

**Response:** Returns the created user and account details, including a flag indicating the welcome email was sent.

### Email Template Management

Email templates are stored in the `email_templates` table and rendered at send-time using `{{variable}}` placeholders. Super admins can manage them via the admin API.

**Template types:**
| Type | Purpose | Default Seed |
|------|---------|-------------|
| `welcome` | Sent on account creation | Pre-loaded |
| `team_invite` | Sent when inviting team members | Pre-loaded |
| `password_reset` | Password reset emails | Available for creation |
| `custom` | Custom use cases | Available for creation |

**API endpoints:**

- `GET /api/v1/admin/email-templates` — List all templates
- `POST /api/v1/admin/email-templates` — Create a new template
- `PUT /api/v1/admin/email-templates/{id}` — Update an existing template
- `DELETE /api/v1/admin/email-templates/{id}` — Delete a template

**Create/Update template body:**
```json
{
  "name": "Welcome Email",
  "subject": "Welcome to {{account_name}}!",
  "body": "Hi {{name}},\n\nWelcome to {{account_name}}. Your temporary password is: {{temp_password}}",
  "html_body": "<h1>Welcome!</h1><p>Hi {{name}}...</p>",
  "template_type": "welcome",
  "is_html": true,
  "is_default": false
}
```

**Available template variables:**
- `{{name}}` — Recipient's name
- `{{email}}` — Recipient's email
- `{{account_name}}` — Account/company name
- `{{temp_password}}` — Temporary password (welcome/team_invite only)
- `{{inviter_name}}` — Name of the person who sent the invite (team_invite only)
- `{{login_url}}` — Login URL

### Email Sending System

All outgoing emails use a DB-backed template engine:

```
send_email(state, to, template_type, vars)
```

- Looks up the template by `template_type` and renders `{{variable}}` placeholders
- Supports both HTML and plain-text modes via the `is_html` flag
- Falls back to inline hardcoded templates if the DB template is missing
- Sends via the configured SMTP provider

To test email delivery:
```bash
# Check the email_templates table for seeded templates
psql -d workflowswift -c "SELECT template_type, name FROM email_templates;"

# Verify SMTP config
systemctl status workflowswift-api.service | grep smtp
```
