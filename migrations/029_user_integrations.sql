-- Migration: 029_user_integrations
-- User-level integration connections (BYOK + native)
-- Separate from provider_keys (which is really tenant-level system keys)
-- This is what the user sees and configures in their Integration Center

CREATE TABLE IF NOT EXISTS user_integrations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    provider VARCHAR(64) NOT NULL,
    provider_label VARCHAR(128) NOT NULL DEFAULT '',
    integration_type VARCHAR(20) NOT NULL DEFAULT 'byok',  -- 'byok', 'native', 'engine'
    api_key_encrypted TEXT,
    base_url VARCHAR(512),
    config JSONB DEFAULT '{}',
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_health_status VARCHAR(20),  -- 'connected', 'error', 'pending'
    last_health_check_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, provider)
);

CREATE INDEX idx_user_integrations_user ON user_integrations(user_id);
CREATE INDEX idx_user_integrations_tenant ON user_integrations(tenant_id);
CREATE INDEX idx_user_integrations_provider ON user_integrations(provider);

-- Seed the native SwiftSoftware integrations as always-available
-- These don't need API keys — they're native products
INSERT INTO available_providers (key, name, description, requires_base_url, icon) VALUES
    ('coreswift', 'CoreSwift (CRM)', 'Full CRM — contacts, leads, deals, notes, activity tracking', false, 'users'),
    ('funnelswift', 'FunnelSwift', 'Landing pages, form submissions, lead routing', false, 'layers'),
    ('incentiveswift', 'IncentiveSwift', 'Rewards, loyalty, referrals, commission campaigns', false, 'award'),
    ('openclaw', 'OpenClaw (Engine)', 'Your own OpenClaw instance for workflow reasoning', true, 'cpu'),
    ('smtp', 'SMTP (Custom)', 'Custom SMTP server for transactional email', true, 'mail')
ON CONFLICT (key) DO NOTHING;

-- Add send_template action for SendGrid
INSERT INTO integration_destinations (provider, action_key, action_label, destination_type, destination_label, sort_order) VALUES
    ('sendgrid', 'send_template_email', 'Send Template Email', 'email_template', 'Email Template', 3)
ON CONFLICT (provider, action_key, destination_type) DO NOTHING;
