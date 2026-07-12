-- Migration: provider_keys
-- Creates tables for dynamic API key management and seeds available providers

-- Table: provider_keys (per-tenant encrypted API key storage)
CREATE TABLE IF NOT EXISTS provider_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    provider VARCHAR(64) NOT NULL,
    api_key TEXT NOT NULL,
    base_url VARCHAR(512),
    metadata JSONB DEFAULT '{}',
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(tenant_id, provider)
);

-- Table: available_providers (dropdown reference data)
CREATE TABLE IF NOT EXISTS available_providers (
    key VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    description TEXT,
    requires_base_url BOOLEAN DEFAULT false,
    requires_metadata JSONB DEFAULT '[]',
    icon VARCHAR(32)
);

-- Seed available_providers
INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon) VALUES
    ('sam_gov', 'SAM.gov', 'System for Award Management — federal contract/grant data', false, '["api_key"]', 'building'),
    ('nexweave', 'Nexweave', 'Personalized video generation for outreach campaigns', true, '["api_key"]', 'video'),
    ('sendiio', 'Sendiio', 'Cold email delivery and analytics platform', false, '["api_key", "from_email"]', 'mail'),
    ('letterman', 'Letterman', 'Direct mail automation platform', false, '["api_key"]', 'file-text'),
    ('google_places', 'Google Places API', 'Google Places and Maps data — reviews, listings, details', false, '["api_key"]', 'map-pin'),
    ('yelp', 'Yelp Fusion API', 'Yelp business reviews and listing data', false, '["api_key"]', 'star'),
    ('facebook', 'Facebook Graph API', 'Facebook pages, posts, and ad data', false, '["api_key", "page_id"]', 'facebook'),
    ('linkedin', 'LinkedIn API', 'LinkedIn profile, company, and ad data', false, '["api_key"]', 'linkedin'),
    ('deepseek', 'DeepSeek API', 'DeepSeek LLM for AI text generation', true, '["api_key", "model"]', 'cpu'),
    ('openai', 'OpenAI API', 'GPT-4, GPT-3.5, DALL-E, Whisper, embeddings', true, '["api_key", "model"]', 'zap'),
    ('anthropic', 'Anthropic API', 'Claude models for AI conversations and analysis', true, '["api_key", "model"]', 'zap'),
    ('mailgun', 'Mailgun API', 'Transactional email delivery service', true, '["api_key", "from_email", "domain"]', 'mail'),
    ('twilio', 'Twilio API', 'SMS, voice, and messaging platform', false, '["api_key", "account_sid"]', 'phone'),
    ('hexomatic', 'Hexomatic API', 'Web scraping automation platform', false, '["api_key"]', 'globe')
ON CONFLICT (key) DO NOTHING;
