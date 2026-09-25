-- 027a: provider_keys + available_providers — RENAMED from `provider_keys.sql` (card t_4ebd6f98).
--
-- WHY THE RENAME
--   The name had no numeric prefix, so it sorted LAST of all 69 files ('p' 0x70 > any digit) while
--   four earlier files already needed what it creates: 028/029 read `available_providers`, 036
--   reads it too, and 046/048 ALTER `provider_keys`. On a from-zero build every one of them died
--   with `ERROR: relation "available_providers"/"provider_keys" does not exist`, and the cascade
--   from there is what made 034_rename_tenant_to_account.sql fail — which in turn is why
--   `accounts` never existed and 8 further files failed.
--
--   `027a` sorts after `027_add_surface_id.sql` (now `042a`) and before `028_*`, so both tables
--   exist before the first reader. 034 runs later and renames `provider_keys.tenant_id` -> `aid`
--   plus its UNIQUE/index names, so the pre-034 shape (tenant_id, REFERENCES tenants(id)) is
--   exactly what this file must create.
--
-- PRODUCTION SAFETY (this file is applied once on the next boot, because the ledger has the old
-- name): every statement is idempotent — `CREATE TABLE IF NOT EXISTS` twice and
-- `INSERT ... ON CONFLICT (key) DO NOTHING`, and all 14 seeded keys already exist there (measured:
-- available_providers holds 20 rows, the 14 below plus 6 added out of band), so it writes ZERO rows.
--
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
-- Shape read from the production catalog (card t_4ebd6f98): `category`, `sort_order`, `plan_id` and
-- `is_active` exist there and are created by NO migration, and 045_integration_center_coreswift.sql
-- INSERTs/UPDATEs `is_active` on this table — without the column a from-zero build died with
-- `column "is_active" of relation "available_providers" does not exist`. This file keeps the
-- pre-034 `provider_keys` shape above (tenant_id + REFERENCES tenants) because
-- 034_rename_tenant_to_account.sql renames it; `available_providers` is not touched by 034.
CREATE TABLE IF NOT EXISTS available_providers (
    key VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    description TEXT,
    requires_base_url BOOLEAN DEFAULT false,
    requires_metadata JSONB DEFAULT '[]',
    icon VARCHAR(32),
    category VARCHAR(64),
    sort_order INTEGER DEFAULT 0,
    plan_id UUID,
    is_active BOOLEAN DEFAULT true
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
