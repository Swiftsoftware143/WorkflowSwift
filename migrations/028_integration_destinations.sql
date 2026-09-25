-- Integration Destinations — 3-level cascade for the Integration step type
-- Product → Action → Destination (list, campaign, tag, audience, etc.)

CREATE TABLE IF NOT EXISTS integration_destinations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider VARCHAR(64) NOT NULL REFERENCES available_providers(key),
    action_key VARCHAR(64) NOT NULL,
    action_label VARCHAR(128) NOT NULL,
    destination_type VARCHAR(64) NOT NULL,  -- 'list', 'campaign', 'tag', 'audience', 'pipeline_stage', etc.
    destination_label VARCHAR(128) NOT NULL,
    description TEXT,
    sort_order INT NOT NULL DEFAULT 0,
    UNIQUE(provider, action_key, destination_type)
);

-- ORDER MATTERS (card t_4ebd6f98)
--   `integration_destinations.provider` REFERENCES `available_providers(key)`, and the three
--   providers the destinations below name were seeded at the END of this file — so the destinations
--   insert used to run before the rows it depends on. Every from-zero build died with
--   `violates foreign key constraint "integration_destinations_provider_fkey"`, and this app's own
--   2026-08-09 install silently lost the whole catalogue the same way (the old runner downgraded
--   the error and recorded the file anyway): measured in production today,
--   `integration_destinations` is EMPTY (0 rows) while `available_providers` has 20.
--   The provider seeds therefore come FIRST. Production is unaffected — the file is recorded in
--   `_migrations` and skipped — so this repair reaches fresh installs only.

-- Add coreswift to available_providers
INSERT INTO available_providers (key, name, description, requires_base_url, icon) VALUES
    ('coreswift', 'CoreSwift (CRM)', 'Full CRM — contacts, leads, deals, notes, activity tracking', false, 'users'),
    ('funnelswift', 'FunnelSwift', 'Landing pages, form submissions, lead routing', false, 'layers'),
    ('incentiveswift', 'IncentiveSwift', 'Rewards, loyalty, referrals, commission campaigns', false, 'award')
ON CONFLICT (key) DO NOTHING;

-- Seed native SwiftSoftware + common third-party destinations
-- Provider-tolerance (card t_4ebd6f98): a destination may only be inserted for a provider that
-- exists in `available_providers` — the FK would otherwise abort the whole file. Only the three
-- SwiftSoftware providers seeded above (plus anything 027a seeds) qualify; the third-party providers
-- this list names (hubspot, slack, ...) are in NO app database and are skipped, exactly as they are
-- in production today (measured: integration_destinations = 0 rows).
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
