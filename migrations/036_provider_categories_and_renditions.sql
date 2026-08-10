-- 036_provider_categories_and_renditions.sql
-- Two features:
--   1. Add category column to available_providers for the Integration Hub
--   2. Create account_renditions table for the Rendition Gallery

BEGIN;

-- ============================================================
-- PART 1: Integration Hub Categories
-- ============================================================
ALTER TABLE available_providers ADD COLUMN IF NOT EXISTS category VARCHAR(64);
CREATE INDEX IF NOT EXISTS idx_available_providers_category ON available_providers(category);

-- Assign categories to the existing providers
UPDATE available_providers SET category = 'ai'              WHERE key IN ('openai', 'anthropic', 'deepseek');
UPDATE available_providers SET category = 'video'           WHERE key IN ('nexweave');
UPDATE available_providers SET category = 'email'           WHERE key IN ('mailgun', 'sendiio', 'smtp');
UPDATE available_providers SET category = 'crm'             WHERE key IN ('coreswift');
UPDATE available_providers SET category = 'landing-pages'   WHERE key IN ('funnelswift');
UPDATE available_providers SET category = 'rewards'         WHERE key IN ('incentiveswift');
UPDATE available_providers SET category = 'automation'      WHERE key IN ('hexomatic', 'openclaw');
UPDATE available_providers SET category = 'data'            WHERE key IN ('google_places', 'sam_gov', 'yelp');
UPDATE available_providers SET category = 'social'          WHERE key IN ('facebook', 'linkedin');
UPDATE available_providers SET category = 'sms'             WHERE key IN ('twilio');
UPDATE available_providers SET category = 'direct-mail'     WHERE key IN ('letterman');

-- ============================================================
-- PART 2: Rendition Gallery
-- ============================================================
-- This table stores reference-only data about rendered assets.
-- No files stored — just preview URLs, provider references, and lifecycle tracking.
CREATE TABLE IF NOT EXISTS account_renditions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    aid UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    workflow_id UUID REFERENCES workflows(id) ON DELETE SET NULL,
    instance_id UUID REFERENCES workflow_instances(id) ON DELETE SET NULL,

    -- What created this rendition
    step_type VARCHAR(50) NOT NULL DEFAULT 'render_media',
    step_name VARCHAR(255),

    -- The third-party provider that owns the actual file
    provider VARCHAR(64) NOT NULL,
    provider_asset_id TEXT NOT NULL,       -- asset ID in that system
    provider_asset_url TEXT NOT NULL,     -- where to view it on the provider

    -- Preview (embeddable inside WorkflowSwift)
    preview_url TEXT,                     -- embeddable URL for inline viewing
    thumbnail_url TEXT,

    -- Asset type
    asset_type VARCHAR(32) NOT NULL CHECK (asset_type IN ('video', 'image', 'audio', 'document', 'other')),

    -- Category reference (matches available_providers.category)
    provider_category VARCHAR(64),

    -- Stitching / timeline grouping
    sort_order INTEGER DEFAULT 0,
    parent_rendition_id UUID REFERENCES account_renditions(id) ON DELETE SET NULL,

    -- Lifecycle
    retention_expires_at TIMESTAMPTZ,
    status VARCHAR(32) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'archived', 'expired', 'purged')),
    metadata JSONB DEFAULT '{}',

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for the gallery views
CREATE INDEX idx_account_renditions_aid ON account_renditions(aid);
CREATE INDEX idx_account_renditions_user ON account_renditions(user_id);
CREATE INDEX idx_account_renditions_workflow ON account_renditions(workflow_id);
CREATE INDEX idx_account_renditions_instance ON account_renditions(instance_id);
CREATE INDEX idx_account_renditions_provider ON account_renditions(provider);
CREATE INDEX idx_account_renditions_asset_type ON account_renditions(asset_type);
CREATE INDEX idx_account_renditions_status ON account_renditions(status);
CREATE INDEX idx_account_renditions_retention ON account_renditions(retention_expires_at)
    WHERE status = 'active';

COMMIT;
