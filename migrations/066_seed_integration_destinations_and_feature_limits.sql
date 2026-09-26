-- 066_seed_integration_destinations_and_feature_limits.sql
-- Restores, in an EXISTING install, the two catalogues a from-zero build has and production
-- does not. Kanban t_82d61045 (follow-up to t_4ebd6f98).
--
-- WHY THIS FILE EXISTS
--   This app's 2026-08-09 install ran with the OLD migration runner, which swallowed a file's
--   error and still recorded it in `_migrations`. Two shipped files therefore never took effect
--   in production while taking effect on every fresh install:
--     * 028_integration_destinations.sql inserted its 18-row destination catalogue BEFORE the
--       `available_providers` rows its FK needs -> 23503, whole insert lost.
--     * 031_multi_industry_dashboards.sql 4-row `feature_limits` seed failed on a relation that
--       did not exist yet at its position in that run.
--   Both files are recorded in production's ledger, so the runner will NEVER re-apply them there
--   (src/db.rs skips a filename already in `_migrations`). This file is that re-application, as a
--   NEW file, so it reaches production on the next boot AND is a no-op on a fresh build (both
--   statements are guarded and idempotent; a fresh build already holds 18 + 4 rows).
--
-- WHY THESE ROWS BELONG TO THE PRODUCT (measured, not judged)
--   * integration_destinations: the catalogue is authored in the repository (028) and read by the
--     mounted route `GET /api/v1/integration-destinations` (src/handlers/integration_center_handler.rs
--     -> provider => actions => destination types). Live BEFORE this file: `actions=0` for
--     coreswift, funnelswift and incentiveswift — production's Integration Center cascade was empty
--     while a fresh install lists 18 destinations. AFTER: 9 + 4 + 5 actions.
--   * feature_limits: `get_plan_capabilities` (src/handlers/plan_handler.rs) reads
--     `max_industries` DIRECTLY from this table and falls back to `.unwrap_or(1)` when no row
--     exists, so live answered `max_industries=1` for a PROFESSIONAL account whose own
--     `plan_tiers.features` says 3. The values below are identical to that JSONB (free 1,
--     starter 3, professional 3, enterprise -1), so this file changes the served number and
--     nothing else — it cannot change what any plan is entitled to beyond agreeing with it.
--
-- OPERATOR-OWNED ROWS: NOT SEEDED HERE (see migrations/README.md, same card)
--   * admin_settings.workflowswift_site — written by the operator through PUT /api/v1/admin/site
--     on 2026-08-11; GET /api/v1/admin/site merges `default_site_settings()` with the row, so a
--     fresh install serves the defaults and needs no seed.
--   * email_templates 'Default Welcome Email' (aid NULL, welcome, created 2026-08-08, before the
--     shipped 012 rows) — measurably SHADOWED: the lookup in src/email.rs orders by
--     `is_default DESC, created_at DESC LIMIT 1`, so both live and a fresh build resolve the
--     newer `Welcome Email` row from 012. Seeding it would add an inert row.
--   * users admin@swiftsoftware.com (040_seed_admin_user.sql) — needed by a fresh install for a
--     first login; production deliberately has no `role='admin'` row (decided in t_2255fdea,
--     commit 3a95468).
--
-- PROOF (audit /opt/swift/audits/t_82d61045/)
--   from-zero: 75/75 files rc=0, row counts unchanged, catalog parity exact.
--   restored copy of production: applied twice -> +18/+4 rows on the first pass, ZERO change on
--   the second, every other table's fingerprint identical.
--
-- IDEMPOTENT + FAIL-SAFE: both statements are `INSERT ... SELECT ... WHERE NOT EXISTS`
-- (or ON CONFLICT DO NOTHING) and the provider guard means a destination can never violate
-- `integration_destinations_provider_fkey`, so this file cannot abort a build.

-- Seed the 18 native destinations (verbatim from 028; see that file's ORDER MATTERS header).
-- Provider-tolerance: a destination may only be inserted for a provider that exists in
-- `available_providers` — the FK would otherwise abort the whole file. The three SwiftSoftware
-- providers all exist in production; most third-party providers this list names (hubspot, slack,
-- ...) exist in NO app database and are skipped by the guard, which is why the catalogue is 18
-- rows and not 46.
INSERT INTO integration_destinations (provider, action_key, action_label, destination_type, destination_label, sort_order)
SELECT v.provider, v.action_key, v.action_label, v.destination_type, v.destination_label, v.sort_order
FROM (VALUES

    -- CoreSwift
    ('coreswift', 'create_contact', 'Create Contact', 'list', 'List', 1),
    ('coreswift', 'add_lead', 'Add Lead', 'list', 'List', 1),
    ('coreswift', 'add_lead', 'Add Lead', 'tags', 'Tags', 2),
    ('coreswift', 'update_deal', 'Update Deal', 'pipeline_stage', 'Pipeline Stage', 1),
    ('coreswift', 'add_note', 'Add Note', 'contact', 'Contact', 1),
    ('coreswift', 'add_note', 'Add Note', 'category', 'Category', 2),
    ('coreswift', 'list_contacts', 'List Contacts', 'list', 'List', 1),
    ('coreswift', 'lookup_contact', 'Lookup Contact', 'lookup_field', 'Lookup by Email', 1),
    ('coreswift', 'trigger_webhook', 'Trigger Webhook', 'webhook_event', 'Event Type', 1),

    -- FunnelSwift
    ('funnelswift', 'route_lead', 'Route Lead', 'tags', 'Tags', 1),
    ('funnelswift', 'export_submissions', 'Export Submissions', 'landing_page', 'Landing Page', 1),
    ('funnelswift', 'export_submissions', 'Export Submissions', 'tags', 'Tags', 2),
    ('funnelswift', 'count_submissions', 'Count Submissions', 'tags', 'Tags', 1),

    -- IncentiveSwift
    ('incentiveswift', 'issue_reward', 'Issue Reward', 'campaign', 'Campaign', 1),
    ('incentiveswift', 'trigger_milestone', 'Trigger Milestone', 'campaign', 'Campaign', 1),
    ('incentiveswift', 'trigger_milestone', 'Trigger Milestone', 'milestone_level', 'Milestone Level', 2),
    ('incentiveswift', 'check_balance', 'Check Balance', 'campaign', 'Campaign', 1),
    ('incentiveswift', 'list_rewards', 'List Rewards', 'campaign', 'Campaign', 1),

    -- Mailchimp
    ('mailchimp', 'add_subscriber', 'Add Subscriber', 'audience', 'Audience', 1),
    ('mailchimp', 'trigger_automation', 'Trigger Automation', 'audience', 'Audience', 1),
    ('mailchimp', 'trigger_automation', 'Trigger Automation', 'automation_email', 'Automation Email', 2),

    -- ActiveCampaign
    ('activecampaign', 'create_contact', 'Create Contact', 'list', 'List', 1),
    ('activecampaign', 'create_contact', 'Create Contact', 'tags', 'Tags', 2),
    ('activecampaign', 'trigger_automation', 'Trigger Automation', 'automation', 'Automation', 1),
    ('activecampaign', 'add_tag', 'Add Tag', 'tag', 'Tag', 1),

    -- ConvertKit
    ('convertkit', 'subscribe', 'Subscribe', 'form', 'Form', 1),
    ('convertkit', 'subscribe', 'Subscribe', 'tag', 'Tag', 2),
    ('convertkit', 'add_tag', 'Add Tag', 'tag', 'Tag', 1),
    ('convertkit', 'add_to_sequence', 'Add to Sequence', 'sequence', 'Sequence', 1),

    -- HubSpot
    ('hubspot', 'create_contact', 'Create Contact', 'list', 'List', 1),
    ('hubspot', 'create_deal', 'Create Deal', 'pipeline', 'Pipeline', 1),
    ('hubspot', 'create_deal', 'Create Deal', 'stage', 'Stage', 2),
    ('hubspot', 'add_to_sequence', 'Add to Sequence', 'sequence', 'Sequence', 1),

    -- Salesforce
    ('salesforce', 'create_contact', 'Create Contact', 'campaign', 'Campaign', 1),
    ('salesforce', 'create_lead', 'Create Lead', 'campaign', 'Campaign', 1),

    -- SendGrid
    ('sendgrid', 'add_to_list', 'Add to List', 'list', 'List', 1),
    ('sendgrid', 'send_campaign', 'Send Campaign', 'segment', 'Segment', 1),

    -- Slack
    ('slack', 'send_message', 'Send Message', 'channel', 'Channel', 1),

    -- Discord
    ('discord', 'send_message', 'Send Message', 'channel', 'Channel', 1),

    -- Google Sheets
    ('google_sheets', 'append_row', 'Append Row', 'spreadsheet', 'Spreadsheet', 1),
    ('google_sheets', 'append_row', 'Append Row', 'sheet_tab', 'Sheet Tab', 2),

    -- Stripe
    ('stripe', 'create_customer', 'Create Customer', 'defaults', 'Default', 1),
    ('stripe', 'create_invoice', 'Create Invoice', 'product', 'Product', 1),
    ('stripe', 'create_invoice', 'Create Invoice', 'price_id', 'Price ID', 2)
) AS v(provider, action_key, action_label, destination_type, destination_label, sort_order)
WHERE EXISTS (SELECT 1 FROM available_providers ap WHERE ap.key = v.provider)
ON CONFLICT (provider, action_key, destination_type) DO NOTHING;

-- The same 4 rows 031 seeds (verbatim). NOT EXISTS, not ON CONFLICT: feature_limits has no unique
-- index on (plan_id, feature_key), so this is the only shape that is idempotent on both databases.
INSERT INTO feature_limits (plan_id, feature_key, limit_value)
SELECT p.id, 'max_industries',
    CASE 
        WHEN p.slug = 'free' THEN 1
        WHEN p.slug = 'starter' THEN 3
        WHEN p.slug = 'professional' THEN 3
        WHEN p.slug = 'enterprise' THEN -1
        ELSE 1
    END
FROM plan_tiers p
WHERE p.slug IN ('free', 'starter', 'professional', 'enterprise')
  AND NOT EXISTS (
    SELECT 1 FROM feature_limits fl 
    WHERE fl.plan_id = p.id AND fl.feature_key = 'max_industries'
  );
