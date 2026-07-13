-- Migration 037: Admin settings & retention configuration
-- Adds global admin settings table and plan feature definitions

-- Global admin settings (key-value with JSONB values)
CREATE TABLE IF NOT EXISTS admin_settings (
    key VARCHAR(255) PRIMARY KEY,
    value JSONB NOT NULL DEFAULT '{}'::jsonb,
    description TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

-- Plan feature definitions (what features exist and their types)
CREATE TABLE IF NOT EXISTS plan_feature_definitions (
    key VARCHAR(255) PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    description TEXT,
    value_type VARCHAR(50) NOT NULL DEFAULT 'numeric', -- 'numeric', 'boolean', 'select', 'text'
    default_value JSONB NOT NULL DEFAULT '0'::jsonb,
    unit VARCHAR(50), -- e.g. 'days', 'seats', 'workflows'
    category VARCHAR(100) NOT NULL DEFAULT 'general', -- 'limits', 'integrations', 'retention', 'support', 'access'
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_visible BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed feature definitions (these define what shows up in the plan editor)
INSERT INTO plan_feature_definitions (key, label, description, value_type, default_value, unit, category, sort_order)
VALUES
    ('max_workflows', 'Max Workflows', 'Maximum number of active workflows', 'numeric', '3', 'workflows', 'limits', 1),
    ('max_instances', 'Max Executions', 'Maximum monthly workflow executions', 'numeric', '10', 'executions', 'limits', 2),
    ('max_users', 'Team Seats', 'Maximum number of team members', 'numeric', '2', 'seats', 'limits', 3),
    ('max_clients', 'Max Clients', 'Maximum number of clients', 'numeric', '5', 'clients', 'limits', 4),
    ('max_templates', 'Max Templates', 'Maximum number of custom templates', 'numeric', '3', 'templates', 'limits', 5),
    ('max_automations', 'Max Automations', 'Maximum number of scheduled automations', 'numeric', '1', 'automations', 'limits', 6),
    ('max_integrations', 'Max Integrations', 'Maximum number of third-party integrations', 'numeric', '2', 'integrations', 'limits', 7),
    ('max_api_keys', 'Max API Keys', 'Maximum number of API keys', 'numeric', '1', 'keys', 'limits', 8),
    ('max_portfolio', 'Max Portfolio Companies', 'Maximum number of portfolio companies', 'numeric', '0', 'companies', 'limits', 9),
    ('max_industries', 'Max Industries', 'Maximum number of industries per account', 'numeric', '1', 'industries', 'limits', 10),
    ('retention_days', 'Data Retention', 'How long execution data is retained', 'numeric', '30', 'days', 'retention', 11),
    ('n8n_deploy', 'n8n Deployment', 'Ability to deploy workflows to n8n', 'boolean', 'false', NULL, 'access', 12),
    ('api_access', 'API Access', 'Access to REST API', 'boolean', 'false', NULL, 'access', 13),
    ('custom_branding', 'Custom Branding', 'White-label branding options', 'boolean', 'false', NULL, 'access', 14),
    ('priority_support', 'Priority Support', 'Priority email and chat support', 'boolean', 'false', NULL, 'support', 15),
    ('dedicated_support', 'Dedicated Support', 'Dedicated account manager', 'boolean', 'false', NULL, 'support', 16),
    ('sla_guarantee', 'SLA Guarantee', 'Service level agreement guarantee', 'boolean', 'false', NULL, 'support', 17),
    ('audit_logs', 'Audit Logs', 'Access to audit log history', 'boolean', 'false', NULL, 'access', 18),
    ('custom_reports', 'Custom Reports', 'Custom report builder access', 'boolean', 'false', NULL, 'access', 19),
    ('webhook_export', 'Webhook Exports', 'Export workflow data via webhooks', 'boolean', 'true', NULL, 'integrations', 20),
    ('csv_export', 'CSV/Excel Export', 'Export data to CSV or Excel', 'boolean', 'true', NULL, 'integrations', 21),
    ('google_sheets', 'Google Sheets Sync', 'Sync data to Google Sheets', 'boolean', 'false', NULL, 'integrations', 22)
ON CONFLICT (key) DO NOTHING;

-- Seed default admin settings
INSERT INTO admin_settings (key, value, description)
VALUES
    ('retention', '{"default_days": 90, "max_days": 365, "min_days": 1}', 'Global data retention policy settings'),
    ('signup', '{"allow_signup": true, "require_approval": false, "default_plan": "free"}', 'Account signup and registration settings'),
    ('branding', '{"app_name": "WorkflowSwift", "support_email": "support@workflowswift.com"}', 'Application branding and contact settings'),
    ('billing', '{"enabled": false, "currency": "USD", "tax_rate": 0, "grace_period_days": 7}', 'Billing and subscription settings'),
    ('security', '{"max_login_attempts": 5, "lockout_minutes": 15, "require_2fa": false, "password_min_length": 8}', 'Security policy settings'),
    ('limits', '{"max_accounts": 0, "max_tenants_per_account": 1, "api_rate_limit": 100}', 'Global system limits')
ON CONFLICT (key) DO NOTHING;

-- Add retention_expires_at to accounts table for per-account retention tracking
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS retention_days INTEGER DEFAULT 90;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS retention_purge_at TIMESTAMPTZ;

-- Add feature limit columns to plan_tiers for quick lookup (alongside JSON features)
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS max_workflows INTEGER DEFAULT 3;
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS max_users INTEGER DEFAULT 2;
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS retention_days INTEGER DEFAULT 30;
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS can_export BOOLEAN DEFAULT true;
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS can_deploy_n8n BOOLEAN DEFAULT false;
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS has_api_access BOOLEAN DEFAULT false;
