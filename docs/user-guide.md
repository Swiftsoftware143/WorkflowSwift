# WorkflowSwift — Visual Step Builder User Guide

## Overview
The Visual Step Builder lets you compose workflows step-by-step using a drag-free card-based pipeline editor. Each workflow is a sequence of steps that execute in order.

### Troubleshooting

**Issue: I don't see the Step Builder at all**
- Make sure you're logged into WorkflowSwift at [workflowswift.com](https://workflowswift.com)
- Check that your account has active subscription status — expired trials hide the builder
- Clear browser cache or try a private/incognito window if the page loads but buttons don't respond

**Issue: My workflow isn't listed after I create it**
- Confirm you saved the workflow before navigating away
- Use the **Surface** filter at the top of the Workflows page; it defaults to "All Surfaces" but if you changed it previously, your workflow might be filtered out
- Refresh the page and check both **All** and **Drafts** tabs if available

---

## Integration Center — Your Connections

Your account comes with an **Integration Center** — this is where you manage how WorkflowSwift connects to other tools, whether they're other SwiftSoftware products or external services you already use.

### Where to find it
- **User menu** (top-right corner with your avatar/name) → **API Keys** — keys other tools use to connect *to* WorkflowSwift
- **User menu** → **Integrations** — where you connect WorkflowSwift *to* other tools
- Both are also in **Settings**

### Your auto-generated API keys (User menu → API Keys)
Every account gets these automatically. Use them when setting up external tools that need to talk to WorkflowSwift:

| Key | What it's for |
|-----|--------------|
| **Primary API Key** | Main auth token. Give this to Zapier, custom scripts, or any app that needs to call the WorkflowSwift API as you. |
| **Webhook Secret** | HMAC secret for verifying incoming webhooks. External services sign their requests with this so the system knows they're legit. |
| **Surface Token** | Token for surface-specific integrations (e.g., your CRM surface connecting back to workflows). |

Keys are shown masked — click to copy the full value. You can regenerate any key (old one stops working immediately) or revoke it.

### Native SwiftSoftware integrations (built-in, zero setup)
If you have an account with any SwiftSoftware product, it's already connected here — no API keys to paste:

| Product | What connects |
|---------|--------------|
| **CoreSwift (CRM)** | Contacts, leads, deals, activity logging — your workflows can read/write your CRM data |
| **FunnelSwift** | Landing page form submissions trigger workflows, data flows to your CRM |
| **IncentiveSwift** | Reward triggers, loyalty points, referral actions |

Each native integration shows as a toggle. Turn it on/off without losing your credentials. If you don't have an account yet, you'll see a **Get Started** link to sign up.

### Third-party integrations (bring your own key)
For services outside SwiftSoftware, paste your own credentials:

| Service | What to enter | Used in workflows for |
|---------|-------------|----------------------|
| **OpenAI** | Your API key | AI Prompt steps |
| **Anthropic** | Your API key | AI Prompt steps |
| **OpenClaw (BYOK)** | Gateway URL + auth token | Route workflows through your own OpenClaw to run the whole system |
| **n8n** | Your n8n URL + API key | Custom automation nodes |
| **Mailchimp** | API key + audience ID | Email and audience steps |
| **ActiveCampaign** | API key + account URL | Auto-responder steps |
| **ConvertKit** | API key | Auto-responder steps |
| **HubSpot** | OAuth login or private app token | CRM steps |
| **Salesforce** | OAuth login or API key | CRM steps |
| **SendGrid** | API key | Email steps |
| **SMTP (custom)** | Server, port, login, password | Email steps |
| **Browserbase / Playwright** | Endpoint URL | Playwright browser automation steps |

### How your workflow steps use these connections
Every step automatically pulls the right credentials — you set them once and all your workflows inherit them:

| Step | Uses |
|------|------|
| 🤖 **AI Prompt** | Your OpenAI or Anthropic key (or the platform's default if you haven't set one) |
| 🌐 **HTTP Request** | Your primary API key for calls to SwiftSoftware tools; whatever auth you configure for external APIs |
| 🔗 **Integration** | Your CRM connection (CoreSwift or HubSpot), auto-responder, or platform connector |
| 📧 **Email** | Your SMTP or SendGrid configuration |
| 🔔 **Notification** | Your surface token |
| 🪝 **Webhook** | Your webhook secret for payload verification |
| 🎭 **Playwright** | Your browser automation endpoint |

### Connection health
Each integration shows its live status:
- ✅ **Connected** — working
- ⚠️ **Error** — something's wrong (with an error message)
- ⚪ **Disabled** — you toggled it off
- 🔵 **Pending** — saved but not tested yet

Click **Test Connection** to check credentials before saving.

### Managing your keys and integrations
1. Open **User menu → API Keys** to copy, regenerate, or revoke your auto-generated keys
2. Open **User menu → Integrations** to add third-party services and toggle native integrations
3. Use **Test Connection** before saving
4. Toggle any integration on/off without re-entering credentials

### Troubleshooting

**Issue: API key shows as masked and I can't copy it**
- Click directly on the masked text — it should copy the full value to your clipboard
- If clicking doesn't work, try clicking the eye icon (👁️) to reveal the key first, then copy manually
- Still stuck? Regenerate the key — the new one will be fully copyable

**Issue: "Test Connection" fails for my third-party integration**
- Double-check you pasted the exact API key — extra spaces or missing characters are easy to miss
- For OAuth services (HubSpot, Salesforce), you may need to re-authenticate if the token expired
- Check that external account hasn't been suspended or rate-limited (e.g., OpenAI billing issues)
- Verify your SMTP server allows connections from WorkflowSwift's IP range

**Issue: A native SwiftSoftware integration shows "Error" status**
- The most common cause is an expired session between products — toggle the integration off, wait 10 seconds, then toggle it back on
- If the problem persists, log out of all SwiftSoftware products and log back in
- Contact support if the error message references an account mismatch

**Issue: I regenerated my Primary API Key and now my existing workflows are failing**
- Regeneration invalidates the old key immediately — any external tool (Zapier, custom script) using the old key will break
- Update the key in every external tool that was using it
- Consider creating a separate API key per tool instead of sharing the primary key so future regenerations don't cause a cascade of failures

**Issue: The integration list is missing a service I need**
- Double-check the search/filter bar at the top of the Integrations page
- Some services are region-locked — check that your account region matches
- Request additional integrations via the feedback option in the user menu

---

## Using SwiftSoftware Products with WorkflowSwift

When a workflow step needs a CRM, auto-responder, or reward system, you use the **Integration** step type. Each product has its own levels — like Zapier or Make — so you can drill down to the exact list, campaign, or tag.

### The pattern

Every integration follows a cascade:

```
Select a Product → Select an Action → Select the Destination
```

The third level is dynamic — it fetches your actual data from that product. For example, when you pick CoreSwift as the product and "Create Contact" as the action, the next dropdown shows *your* CoreSwift lists.

---

### Integration levels by product

#### CoreSwift (CRM)
| Action | Then pick |
|--------|----------|
| Create Contact | **List** to assign them to (e.g. "New Leads", "VIP Clients") |
| Add Lead | **List** + optional **Tags** |
| Update Deal | **Pipeline Stage** |
| Add Note | **Contact** (by lookup) + **Category** |
| List Contacts | **List** to pull from |

#### FunnelSwift (Landing Pages)
| Action | Then pick |
|--------|----------|
| Route Lead | **Tag match** to determine destination |
| Export Submissions | **Landing Page** + **Tags** to filter by |
| Count Submissions | **Tags** to filter by |

#### IncentiveSwift (Rewards & Loyalty)
| Action | Then pick |
|--------|----------|
| Issue Reward | **Campaign** (e.g. "Referral Bonus Q3", "VIP Anniversary") |
| Trigger Milestone | **Campaign** → then **Milestone Level** |
| Check Balance | **Campaign** |
| List Rewards | **Campaign** |

#### Mailchimp (third-party)
| Action | Then pick |
|--------|----------|
| Add Subscriber | **Audience** (e.g. "Monthly Newsletter") |
| Trigger Automation | **Audience** → then **Automation Email** |

#### ActiveCampaign (third-party)
| Action | Then pick |
|--------|----------|
| Create Contact | **List** + **Tags** |
| Trigger Automation | **Automation** |
| Add Tag | **Tag** from your account |

#### HubSpot (third-party)
| Action | Then pick |
|--------|----------|
| Create Contact | **List** |
| Create Deal | **Pipeline** → then **Stage** |
| Add to Sequence | **Sequence** |

#### SendGrid (third-party)
| Action | Then pick |
|--------|----------|
| Add to List | **List** |
| Send Campaign | **Segment** |

---

### Example 1: FunnelSwift form → CoreSwift list → IncentiveSwift campaign

A visitor fills out a FunnelSwift landing page. The workflow creates them in a specific CoreSwift list, then issues a referral reward from a specific IncentiveSwift campaign.

| Step | Type | Selections |
|------|------|-----------|
| 1 | Webhook | Triggered by FunnelSwift form. Payload: `{{name}}`, `{{email}}`, `{{phone}}`, `{{source}}` |
| 2 | **Integration** | **CoreSwift** → Create Contact → **List:** "New Leads" → Map: name, email, phone, source |
| 3 | **Integration** | **IncentiveSwift** → Issue Reward → **Campaign:** "Referral Bonus Q3" → Map: email → `{{email}}` |
| 4 | Notification | "New lead from {{source}}: {{name}}" |

---

### Example 2: Deal won → commission from specific campaign

A CoreSwift deal moves to "Won." The workflow issues the commission from the "Sales Commissions" campaign and emails the rep.

| Step | Type | Selections |
|------|------|-----------|
| 1 | Webhook | CoreSwift deal update. Payload: `{{deal_name}}`, `{{deal_value}}`, `{{sales_rep_email}}`, `{{status}}` |
| 2 | Condition | Only run if `{{status}}` = "won" |
| 3 | **Integration** | **IncentiveSwift** → Issue Reward → **Campaign:** "Sales Commissions" → Map: amount, recipient |
| 4 | Email | Receipt to `{{sales_rep_email}}`: "Commission paid for {{deal_name}}" |
| 5 | Data Card | Display summary |

---

### Example 3: Tag-based routing to CRM lists

Two FunnelSwift landing page variants tagged differently send leads to separate CoreSwift lists.

| Step | Type | Selections |
|------|------|-----------|
| 1 | Webhook | FunnelSwift submission. Payload: `{{email}}`, `{{name}}`, `{{tags}}`, `{{page_variant}}` |
| 2 | Condition | Branch: `{{page_variant}}` = "variant-a" |
| 3a | **Integration** (var-a) | **CoreSwift** → Create Contact → **List:** "A/B Variant A" → Map: email, name |
| 3b | **Integration** (var-b) | **CoreSwift** → Create Contact → **List:** "A/B Variant B" → Map: email, name |
| 4 | Notification | "New {{page_variant}} lead" |

---

### Example 4: IncentiveSwift campaign milestone → CRM note

A customer hits a milestone in a specific IncentiveSwift campaign. The workflow logs it as a categorized note on their CoreSwift contact.

| Step | Type | Selections |
|------|------|-----------|
| 1 | Webhook | IncentiveSwift milestone. Payload: `{{customer_email}}`, `{{milestone_name}}`, `{{campaign_name}}` |
| 2 | **Integration** | **CoreSwift** → Lookup Contact → Map: email → `{{customer_email}}` → returns `{{contact_id}}` |
| 3 | **Integration** | **CoreSwift** → Add Note → **Category:** "Milestones" → Map: contact_id, content |
| 4 | Data Card | Display achievement |

---

### Example 5: Weekly cross-product dashboard

A scheduled workflow pulls stats from each product, filtered by the relevant destinations, and summarizes them.

| Step | Type | Selections |
|------|------|-----------|
| 1 | **Integration** | **CoreSwift** → List Contacts → **List:** "All Active" |
| 2 | **Integration** | **FunnelSwift** → Count Submissions → **Tags:** "campaign-q3" |
| 3 | **Integration** | **IncentiveSwift** → List Rewards → **Campaign:** "Referral Bonus Q3" |
| 4 | AI Prompt | Summarize counts |
| 5 | Dashboard Push | Push widget |

### Troubleshooting

**Issue: The dynamic dropdown at the "Destination" level is blank or stuck loading**
- Refresh the page and try again — sometimes the token used to fetch your data needs a fresh session
- Check that the product integration is showing a green "Connected" status in the Integration Center
- The dropdown fetches live data from the service; if CoreSwift or HubSpot is experiencing an outage, the dropdown will remain empty
- Make sure you have at least one list/campaign/audience created on the provider side — empty accounts display no options

**Issue: I selected a list but the step errors at runtime saying the destination doesn't exist**
- The list may have been renamed or deleted after you configured the step — open the Integration step and re-select the list from the dropdown
- If you have multiple WorkflowSwift accounts, confirm you're using the right one; the list IDs are tied to each account

**Issue: My FunnelSwift form submission doesn't trigger the workflow**
- Verify the webhook step's trigger URL is pasted into FunnelSwift's form settings correctly
- Check that the webhook payload field names (`{{name}}`, `{{email}}`) match exactly what FunnelSwift sends — a mismatch causes silent failures
- Use a Data Card or Notification step early in the pipeline to debug incoming payload values

**Issue: IncentiveSwift reward isn't issued despite the step completing**
- Confirm the campaign is active (not ended or paused) in IncentiveSwift
- Verify the `{{email}}` or recipient mapping resolves to a valid user in the IncentiveSwift campaign
- Some campaigns have spending caps or per-user limits — check if those are exhausted

---

## Surface Filter — Filtering by Surface

Both the **Workflows** page and **Templates Gallery** include a **Surface** dropdown filter.

1. Go to **Workflows** or **Templates** in the sidebar
2. Look for the **All Surfaces** dropdown next to the search bar
3. Click it to see your options:
   - **All Surfaces** — shows everything (default)
   - **No Surface** — shows only items without a surface assignment
   - Any named surface — shows only items assigned to that surface
4. Select a surface — the list filters instantly

The filter combines with the search bar and Industry filter (on Templates) so you can narrow by surface + keyword + industry at the same time.

### Troubleshooting

**Issue: I created a workflow but it doesn't appear in the list**
- Check the Surface filter — if you previously filtered to a specific surface, new workflows without a surface assignment won't show
- Switch the filter to **All Surfaces** to confirm the workflow exists
- If your workflow was assigned to a surface that was later deleted, select **No Surface** to find it

**Issue: The Surface dropdown is empty — no surfaces listed**
- Surfaces are created by your account admin; if none have been configured, the dropdown will only show "All Surfaces" and "No Surface"
- Ask your admin to create surfaces in Settings → Surfaces
- Templates from the gallery may have pre-assigned surfaces that aren't usable until your admin creates matching ones

---

## Accessing the Step Builder
1. Log into WorkflowSwift at [workflowswift.com](https://workflowswift.com)
2. Go to **Workflows** from the sidebar
3. Click a workflow name or the **Edit Steps** button on any workflow card
4. You'll see the **Step Builder** — a pipeline view of your workflow's steps

### Troubleshooting

**Issue: Clicking a workflow name shows a read-only view, not the Step Builder**
- You may not have edit permissions for that workflow — check with your account admin
- Workflows shared with you via a surface may be view-only; clone the workflow to get an editable copy
- If you just created the workflow, try refreshing the page

**Issue: The "Edit Steps" button is missing from the workflow card**
- Hover over the card — the button may appear on hover
- The workflow may still be in "draft" state and need to be saved first
- Try accessing the workflow by clicking its name and looking for an **Edit Steps** tab on the detail page

---

## The Pipeline View
- Steps display as horizontal cards connected by arrow connectors
- Each card shows: step number, icon (based on type), name, and a brief description
- The last card is always **Add Step** — click it to add a new step

### Troubleshooting

**Issue: The pipeline shows steps in the wrong order**
- Use the **▲** and **▼** buttons on each card to reorder them (see Reordering Steps section)
- If the arrows aren't responding, save the workflow first then try again
- Steps are numbered sequentially — if you deleted a step and the numbers look odd, that's normal; they re-number on the next page load

**Issue: I see a step card with "Unknown Step Type" or a generic icon**
- This usually means the step configuration has data the UI can't parse — open the step in Edit mode
- Check if you manually edited the Config JSON field with an invalid schema
- If the step was imported from a template, the template may reference a step type that's been renamed

**Issue: The arrow connectors between steps are overlapping or hard to read**
- This is a display issue on narrower browser windows — try widening your browser or using a larger screen
- Zoom out (Ctrl/Cmd + -) to see the full pipeline more clearly
- If the layout is broken entirely, refresh the page

---

## Step Types
| Type | Icon | Purpose |
|------|------|---------|
| Data Card | 📊 | Display data inline in the workflow UI |
| HTTP Request | 🌐 | Make API calls to external services |
| Delay | ⏱️ | Pause execution for a set time |
| Condition | 🔀 | Branch logic (if/else) based on data |
| Integration | 🔗 | Connect to third-party tools |
| Email | 📧 | Send email notifications |
| Notification | 🔔 | Send in-app or push notifications |
| Webhook | 🪝 | Trigger or respond to webhooks |
| Playwright | 🎭 | Browser automation (scraping, form fills) |
| AI Prompt | 🤖 | Run LLM prompts with custom inputs |

### Troubleshooting

**Issue: I can't find a step type I need — for example, there's no "Database Query" or "File Upload" step**
- WorkflowSwift has 10 specific step types listed above — there isn't a step for every possible action
- For custom database queries, use the **HTTP Request** step to call your own API endpoint
- For file handling, use a **Playwright** step (browser-based) or an **HTTP Request** to a file-processing service
- Suggest missing step types via the feedback option in the user menu

**Issue: I selected a step type but the form asks for different fields than I expected**
- Each step type has its own form fields — review the Step Configuration Reference section below for exact field descriptions
- Double-check you selected the correct type before filling out the form
- If you're midway through configuration, you can close the modal and start again

---

## Adding a Step
1. Click **+ Add Step** at the end of your pipeline
2. A modal opens with a step type selector — choose one of the 10 types
3. Fill out the guided form fields (changes based on step type)
4. The **Config JSON** field updates automatically as you fill in fields
5. Click **Add Step** — it appears at the end of your pipeline

### Example: Adding an HTTP Request Step
1. Click + Add Step → Select **HTTP Request**
2. Enter a name: "Fetch User Data"
3. Description: "Get user profile from API"
4. URL: `https://api.example.com/users/{{userId}}`
5. Method: GET
6. Headers: `{ "Authorization": "Bearer {{token}}" }`
7. Click **Add Step**

### Troubleshooting

**Issue: The "Add Step" button is grayed out and unclickable**
- Required fields are missing — the modal won't let you submit until all mandatory fields are filled
- Check for red outlines or asterisk indicators on required fields
- If the step type selector is still open, you need to select a step type first before the form appears

**Issue: I filled everything out but clicking "Add Step" does nothing**
- Check for validation errors near the bottom of the form or on the Config JSON field
- If you manually edited the Config JSON, make sure it's valid JSON (use a JSON validator if unsure)
- Browser extensions (especially ad blockers) sometimes interfere with modal forms — try disabling them

**Issue: My step was added but it appears at the wrong position**
- Steps always append to the end of the pipeline
- Use the **▲** and **▼** buttons to move it to the correct position (see Reordering Steps)
- If you need a step between existing steps, add it at the end and reorder — there's no "insert at position" option yet

**Issue: The Config JSON field shows `{}` even though I filled in the form**
- Make sure you tab out of or click away from the last form field before checking the Config JSON — the auto-update triggers on field blur
- If it stays empty, try typing one character in the Config JSON field and deleting it; this sometimes kickstarts the sync
- As a fallback, edit the Config JSON directly using the Step Configuration Reference below

---

## Editing a Step
1. Click the **Edit** (✏️) button on any step card
2. The same modal opens pre-filled with that step's data
3. Make your changes and click **Update Step**

### Troubleshooting

**Issue: The Edit modal shows different data than what I entered**
- If you manually edited the Config JSON after filling the form, the form fields may not reflect those manual changes
- The form fields are one-way sync (form → JSON) — manually editing JSON won't update the form fields
- To fix, use the Config JSON tab/view in the edit modal to see the true stored configuration

**Issue: Clicking "Update Step" says "No changes detected"**
- You need to actually modify a field before the update button registers changes
- If you changed the Config JSON manually, toggle a form field on/off to trigger the save check
- If no changes are truly needed, just close the modal — the data is already saved

---

## Reordering Steps
Use the **▲** and **▼** buttons on each step card to move it up or down in the pipeline.
- The new order saves automatically after each move
- Step numbers update to reflect the new order

### Troubleshooting

**Issue: The ▲ or ▼ buttons don't appear on my step cards**
- The step may be the first or last in the pipeline — the first step only has ▼, the last step only has ▲
- If no arrows appear at all, the workflow may not be editable (see Accessing the Step Builder troubleshooting)
- Try refreshing the page — the buttons may not have loaded correctly

**Issue: I moved a step but the workflow doesn't behave as expected at runtime**
- Check that your Condition steps reference the correct step numbers — moving steps changes the pipeline order
- Variables from previous steps (`{{variable}}`) must come from steps that execute *before* the step that uses them. If you moved a variable's source step below a dependent step, it will be empty at runtime

---

## Deleting a Step
1. Click the **Delete** (🗑️) button on any step card
2. Confirm deletion
3. The step is removed and remaining steps re-number automatically

### Troubleshooting

**Issue: The Delete button is missing**
- Some workflows may be view-only if shared via surface permissions
- Check that you're not in a read-only view (see Accessing the Step Builder troubleshooting)
- If the step is the only step in the workflow, you may need to delete the entire workflow instead

**Issue: I accidentally deleted a step — can I undo it?**
- There is no undo for step deletion
- If you haven't saved or navigated away, try refreshing the page — the browser may still have the previous state cached
- If the deletion persisted, you'll need to re-add the step manually; reference any email notification or template you may have saved
- Consider making a note of your workflow configuration before making significant changes

**Issue: After deleting a step, variables used in later steps are broken**
- If the deleted step provided data that later steps reference via `{{variable}}`, those variables will resolve to empty
- Edit the downstream steps to remove or replace references to the deleted step's output
- The step numbers remain sequential after deletion, but the data from the removed step is gone

---

## Step Configuration Reference

### Data Card
- **Name:** Display name
- **Description:** Optional description
- **Title:** Card title shown in UI
- **Content:** Markdown or HTML content to display
- **Config JSON:** `{ "title": "...", "content": "..." }`

### HTTP Request
- **URL:** Full endpoint URL (supports `{{variables}}`)
- **Method:** GET, POST, PUT, PATCH, DELETE
- **Headers:** JSON object of key-value headers
- **Body (optional):** JSON request body for POST/PUT/PATCH
- **Config JSON:** `{ "url": "...", "method": "GET", "headers": {...}, "body": null }`

### Delay
- **Duration:** Seconds to wait before next step
- **Config JSON:** `{ "duration_seconds": 30 }`

### Condition
- **Variable:** Data field to evaluate (e.g. `{{response.status}}`)
- **Operator:** equals, not_equals, greater_than, less_than, contains
- **Value:** Value to compare against
- **Config JSON:** `{ "variable": "...", "operator": "equals", "value": "..." }`

### Integration
- **Integration Type:** e.g. slack, stripe, google_sheets, discord
- **Action:** What action to perform (send_message, create_record, etc.)
- **Config JSON:** `{ "integration_type": "slack", "action": "send_message", ... }`

### Email
- **To:** Recipient email(s), comma-separated
- **Subject:** Email subject line
- **Body:** Email body (supports variables)
- **Config JSON:** `{ "to": "...", "subject": "...", "body": "..." }`

### Notification
- **Channel:** in_app, push, or email
- **Title:** Notification title
- **Message:** Notification body
- **Config JSON:** `{ "channel": "in_app", "title": "...", "message": "..." }`

### Webhook
- **URL:** Webhook endpoint URL
- **Method:** POST, PUT, PATCH
- **Payload:** JSON payload to send
- **Config JSON:** `{ "url": "...", "method": "POST", "payload": {...} }`

### Playwright
- **URL:** Page URL to automate
- **Action:** screenshot, scrape, fill_form, click, navigate
- **Selector (optional):** CSS selector for targeted actions
- **Config JSON:** `{ "url": "...", "action": "screenshot", "selector": null }`

### AI Prompt
- **Prompt:** The text prompt to send to the LLM
- **Model:** gpt-4, gpt-3.5-turbo, claude-3, etc.
- **Variables:** JSON object of template variables
- **Config JSON:** `{ "prompt": "...", "model": "gpt-4", "variables": {} }`

### Troubleshooting

**Issue: My HTTP Request step returns a 401 or 403 error**
- The URL's endpoint requires authentication — add an `Authorization` header in the Headers field
- If you're calling a SwiftSoftware API, your Primary API Key is used automatically; for external APIs, you need to provide credentials in the headers
- Check that your API key hasn't been regenerated since the workflow was created

**Issue: AI Prompt returns empty or nonsensical results**
- Verify your prompt is complete and not truncated
- Check which model is selected — some models handle complex instructions better than others
- Confirm your OpenAI or Anthropic integration is connected and has available credits
- Template variables (`{{variable}}`) that don't resolve will produce empty spots in the prompt — preview the rendered prompt with a Data Card first

**Issue: The Condition step never evaluates to true**
- Use a Data Card or Notification step earlier in the pipeline to log the actual value of `{{variable}}` — it may be formatted differently than you expect (e.g., `"won"` vs `"Won"`)
- Check the operator: "contains" is case-sensitive, "equals" is also case-sensitive
- Verify the variable path is correct — `{{response.status}}` is not the same as `{{status}}`

**Issue: Email step sends to the wrong recipient or doesn't send at all**
- Check that your SMTP/SendGrid integration is showing "Connected" status
- For multiple recipients, ensure they're comma-separated without extra spaces: `user1@example.com,user2@example.com`
- The Email step sends from the SMTP or SendGrid account you configured — verify that account can send to the target domains
- Check your spam folder — some providers route automated emails to spam

**Issue: Playwright step says "Selector not found" or times out**
- The page may be loading dynamically — CSS selectors need to match elements that exist at render time
- Use browser dev tools (F12) on the target page to verify your selector works
- Some sites block automated browsing — try a different action (e.g., "screenshot" instead of "scrape")
- Increase the Delay step before the Playwright step if the page has lazy-loaded content

**Issue: Webhook step doesn't receive the expected payload**
- Confirm the external service is sending to the correct URL — copy the webhook URL from the step configuration, not from a different workflow
- Check that the Webhook Secret in your Integration Center matches what the external service is using to sign requests
- Use a tool like RequestBin to test what the external service is actually sending before wiring it to a workflow

---

## Tips
- Step config JSON is editable manually if you know the schema
- Use `{{variable}}` syntax to reference data from previous steps
- Workflows must be saved before steps can be added
- The step builder auto-saves reorder changes

### Troubleshooting

**Issue: My `{{variable}}` syntax isn't being replaced at runtime**
- Variable names are case-sensitive — `{{Email}}` is different from `{{email}}`
- The variable must come from a step that runs *before* the step referencing it in the pipeline
- Some step types produce named outputs; others produce a single unnamed output — check the Step Configuration Reference to know what variables are available per type
- Test with a Data Card or Notification step to print all available variables mid-pipeline

**Issue: I manually edited the Config JSON but my step isn't working**
- Open the step in Edit mode and look at the Config JSON field — invalid JSON will silently fail
- Check for trailing commas, missing quotes, or unescaped special characters
- The form fields will show the parsed configuration; if they look blank, the JSON didn't parse correctly
- Copy the Config JSON into a JSON validator (like jsonlint.com) to find syntax errors

**Issue: My workflow runs but I get no output from any steps**
- Add a Data Card as the final step with content like `{{step1}}`, `{{step2}}` to see what each step produces
- Check that each step's configuration is fully filled in — partially configured steps may complete without producing output
- If the workflow was triggered via webhook, verify the webhook actually received a request (check workflow run history)

---

## Team Management

### Role System

Your role determines what you can see and do in WorkflowSwift:

| Role | Access Level |
|------|-------------|
| **super_admin** | Full system access — user creation, email template management |
| **user** | Full access — can create and edit workflows, manage integrations, invite team members |
| **team_member** | Scoped access — permissions are granular and set by the person who invited you |

When you sign up, you're automatically assigned the **user** role. If you were invited by someone, you're a **team_member** with specific permissions.

### Inviting Team Members

Any user can invite others to their workspace. This lets you collaborate on workflows without sharing login credentials.

**Endpoint:** `POST /api/v1/users/invite`

**Request body:**
```json
{
  "name": "Alex Smith",
  "email": "alex@example.com",
  "role": "team_member",
  "permissions": {
    "workflows": ["view", "create", "edit"],
    "templates": ["view", "use"],
    "settings": ["view"]
  }
}
```

**Valid roles for invite:** `team_member` (default). You cannot invite someone as `user` or `super_admin`.

**How it works:**
1. You fill in the invite form with name, email, role, and granular permissions
2. The system sends a real HTML email with a temporary password and login link
3. The invited team member logs in and can change their password
4. Their permissions are immediately enforced on the next request

### Managing Team Permissions

**View your team:** `GET /api/v1/users/team` — lists only users who are team members of your workspace (not your own user account).

**Update permissions:** `PUT /api/v1/users/{id}/permissions` — update granular permissions for a specific team member.

**Request body:**
```json
{
  "permissions": {
    "workflows": ["view"],
    "templates": [],
    "settings": []
  }
}
```

This allows you to lock down or expand what each team member can do at any time. Permissions take effect immediately.

### The Guide Page

Click the **?** icon in the sidebar to access this guide at any time. The guide is context-sensitive — it covers the page you're currently viewing, with comprehensive sections for all features.

### How Roles Affect Visibility

- **Workflows**: Team members with only `view` permission on workflows can see workflow lists and run history but cannot create, edit, or delete workflows
- **Templates**: Team members with only `use` permission can apply templates but cannot create or modify them
- **Settings**: If `settings` is not included in a team member's permissions, the Settings menu will be hidden
- **Integration Center**: Visible to all users and team members, but API key management may be restricted based on permissions

---

## Checkout & Payments

WorkflowSwift now supports **Stripe** and **PayPal** checkout sessions, letting you accept payments directly within your workflows.

### How payments work

1. An **admin** configures one or more payment providers (Stripe/PayPal) with API keys
2. A workflow step calls the checkout API with an amount, currency, and callback URLs
3. The user is redirected to the provider's hosted checkout page
4. The payment is confirmed via webhook — the session is marked `completed` automatically

### Available endpoints (user-facing)

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/checkout/create` | POST | Create a Stripe or PayPal checkout session |
| `/api/v1/checkout/sessions` | GET | List your checkout sessions and their status |

### Creating a checkout session

```json
POST /api/v1/checkout/create
{
  "provider_type": "stripe",
  "purchasable_type": "subscription",
  "purchasable_id": "550e8400-e29b-41d4-a716-446655440000",
  "amount": 29.99,
  "currency": "USD",
  "success_url": "https://app.workflowswift.com/payment/success",
  "cancel_url": "https://app.workflowswift.com/payment/cancel",
  "metadata": {
    "workflow_id": "abc-123",
    "user_email": "user@example.com"
  }
}
```

**Response:** returns a redirect URL to the provider's hosted checkout page and a `session_id` you can use to track the session.

### Session statuses

- `pending` — Created, awaiting user payment
- `completed` — Payment confirmed via webhook
- `expired` — Session expired without payment
- `failed` — Payment was declined or errored

> 💡 Payment providers must be configured by an admin before they appear as available. If you see "No active provider configured", contact your admin.
