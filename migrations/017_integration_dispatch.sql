-- Provider presets — pre-configured base URLs so users only need their API key
CREATE TABLE IF NOT EXISTS integration_provider_presets (
    key TEXT PRIMARY KEY,       -- lookup key: 'mailgun', 'sendgrid', 'sendiio', 'mailchimp', 'coreswift_crm'
    name TEXT NOT NULL,         -- display name: 'Mailgun', 'SendGrid', etc.
    base_url TEXT NOT NULL,     -- API base URL for the provider
    docs_url TEXT,              -- link to API docs
    -- Columns production has and no migration created (card t_4ebd6f98, read from its catalog):
    -- 045_integration_center_coreswift.sql writes `sort_order` and `is_active` here, so without them
    -- a from-zero build died with `column "sort_order" of relation "integration_provider_presets"
    -- does not exist`.
    sort_order INTEGER DEFAULT 0,
    plan_id UUID,
    is_active BOOLEAN DEFAULT true
);
INSERT INTO integration_provider_presets (key, name, base_url, docs_url) VALUES
    ('mailgun', 'Mailgun', 'https://api.mailgun.net/v3', 'https://documentation.mailgun.com'),
    ('sendgrid', 'SendGrid', 'https://api.sendgrid.com/v3', 'https://docs.sendgrid.com'),
    ('sendiio', 'Sendiio', 'https://api.sendiio.com/v2', 'https://sendiio.com/docs'),
    ('mailchimp', 'Mailchimp', 'https://us21.api.mailchimp.com/3.0', 'https://mailchimp.com/developer'),
    ('coreswift_crm', 'CoreSwift CRM', 'http://localhost:8084', 'https://coreswiftcrm.com/docs'),
    ('webhook', 'Custom Webhook', '', NULL)
ON CONFLICT (key) DO NOTHING;

-- Add provider_preset column to integration_targets so users pick from the list
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS provider_preset TEXT REFERENCES integration_provider_presets(key);
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS recipient_email TEXT;
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS cc_emails TEXT[];
