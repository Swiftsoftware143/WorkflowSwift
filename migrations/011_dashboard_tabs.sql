-- WorkflowSwift Dashboard Tabs
-- Schema for Brand Monitor, Competitor Watch, and Prospecting tabs
-- July 3, 2026

-- 1. Dashboard Data Sources (per user/tenant)
CREATE TABLE IF NOT EXISTS dashboard_data_sources (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    source_type VARCHAR(50) NOT NULL, -- 'browser_extension', 'api_google', 'api_facebook', 'api_instagram', 'n8n_webhook', 'rss'
    source_name VARCHAR(255) NOT NULL,
    source_config JSONB NOT NULL DEFAULT '{}', -- API keys, endpoints, credentials
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_sync_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dashboard_sources_tenant ON dashboard_data_sources(tenant_id);

-- 2. Brand Monitor - Brands/Topics being tracked
CREATE TABLE IF NOT EXISTS brand_monitor_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    brand_name VARCHAR(255) NOT NULL,
    keywords TEXT[] NOT NULL DEFAULT '{}',
    sources TEXT[] NOT NULL DEFAULT '{"web","news","social"}', -- where to search
    schedule_cron VARCHAR(100) DEFAULT '0 */6 * * *', -- every 6 hours default
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_brand_monitor_tenant ON brand_monitor_items(tenant_id);

-- 3. Brand Monitor - Results/Mentions
CREATE TABLE IF NOT EXISTS brand_monitor_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_item_id UUID NOT NULL REFERENCES brand_monitor_items(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    source VARCHAR(100) NOT NULL, -- 'google', 'facebook', 'instagram', 'reddit', 'hackernews', 'news', 'rss'
    source_url TEXT NOT NULL,
    title TEXT,
    snippet TEXT,
    sentiment VARCHAR(20), -- 'positive', 'negative', 'neutral', 'mixed'
    sentiment_score REAL, -- -1.0 to 1.0
    published_at TIMESTAMPTZ,
    raw_data JSONB,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_brand_results_item ON brand_monitor_results(brand_item_id);
CREATE INDEX IF NOT EXISTS idx_brand_results_tenant ON brand_monitor_results(tenant_id);

-- 4. Competitor Watch - Competitors being tracked
CREATE TABLE IF NOT EXISTS competitor_watch_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    competitor_name VARCHAR(255) NOT NULL,
    competitor_website TEXT,
    competitor_social JSONB DEFAULT '{}', -- { "facebook": "...", "instagram": "...", "twitter": "..." }
    watch_focus TEXT[] NOT NULL DEFAULT '{"pricing","content","reviews","activity"}',
    schedule_cron VARCHAR(100) DEFAULT '0 */12 * * *', -- every 12 hours default
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_competitor_watch_tenant ON competitor_watch_items(tenant_id);

-- 5. Competitor Watch - Findings/Changes
CREATE TABLE IF NOT EXISTS competitor_watch_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    competitor_id UUID NOT NULL REFERENCES competitor_watch_items(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    change_type VARCHAR(50) NOT NULL, -- 'pricing_change', 'new_content', 'new_review', 'social_post', 'website_change'
    description TEXT,
    source_url TEXT,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    raw_data JSONB,
    alert_sent BOOLEAN NOT NULL DEFAULT false
);

CREATE INDEX IF NOT EXISTS idx_competitor_results_comp ON competitor_watch_results(competitor_id);
CREATE INDEX IF NOT EXISTS idx_competitor_results_tenant ON competitor_watch_results(tenant_id);

-- 6. Prospecting - Search queries
CREATE TABLE IF NOT EXISTS prospecting_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    industry VARCHAR(255) NOT NULL,
    city VARCHAR(255) NOT NULL,
    state VARCHAR(255) NOT NULL,
    search_query TEXT GENERATED ALWAYS AS (industry || ' in ' || city || ', ' || state) STORED,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_prospecting_tenant ON prospecting_items(tenant_id);
CREATE INDEX IF NOT EXISTS idx_prospecting_query ON prospecting_items(industry, city, state);

-- 7. Prospecting - Results (businesses found)
CREATE TABLE IF NOT EXISTS prospecting_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    prospecting_id UUID NOT NULL REFERENCES prospecting_items(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    business_name VARCHAR(255) NOT NULL,
    business_website TEXT,
    business_phone VARCHAR(50),
    business_email VARCHAR(255),
    business_address TEXT,
    source VARCHAR(100) NOT NULL, -- 'google', 'facebook', 'instagram', 'yellowpages', 'browser_extension'
    source_url TEXT,
    social_links JSONB DEFAULT '{}', -- { "facebook": "...", "instagram": "...", "linkedin": "..." }
    rating REAL,
    review_count INTEGER,
    raw_data JSONB,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_prospecting_results_prospect ON prospecting_results(prospecting_id);
CREATE INDEX IF NOT EXISTS idx_prospecting_results_tenant ON prospecting_results(tenant_id);

-- 8. Dashboard Tab Config (connects tabs to user's dashboard layout)
CREATE TABLE IF NOT EXISTS dashboard_tab_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    tab_type VARCHAR(50) NOT NULL, -- 'brand_monitor', 'competitor_watch', 'prospecting'
    tab_label VARCHAR(255) NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_visible BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, tab_type)
);

CREATE INDEX IF NOT EXISTS idx_dashboard_tab_config_tenant ON dashboard_tab_config(tenant_id);

-- 9. Connect dashboard data to workflows
CREATE TABLE IF NOT EXISTS dashboard_workflow_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    dashboard_tab_type VARCHAR(50) NOT NULL,
    source_item_id UUID NOT NULL, -- polymorphic: can reference any of the above tables
    source_table VARCHAR(100) NOT NULL, -- which table source_item_id points to
    workflow_id UUID NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    trigger_on VARCHAR(50) NOT NULL DEFAULT 'new_result', -- 'new_result', 'schedule', 'manual'
    config JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dashboard_workflow_links_tenant ON dashboard_workflow_links(tenant_id);
CREATE INDEX IF NOT EXISTS idx_dashboard_workflow_links_workflow ON dashboard_workflow_links(workflow_id);
