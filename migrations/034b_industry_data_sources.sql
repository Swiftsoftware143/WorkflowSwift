-- 034b: Industry data sources — RENAMED from `013_industry_data_sources.sql` (card t_4ebd6f98).
--
-- WHY THE RENAME
--   The file ALTERs `accounts` (`ADD COLUMN IF NOT EXISTS active_industry_sources`), and `accounts`
--   is created by 034_rename_tenant_to_account.sql, so as `013_*` it ran 21 files too early and a
--   from-zero build died with `ERROR: relation "accounts" does not exist`. `034b` runs immediately
--   after 034 / 034a.
--
-- PRODUCTION SAFETY (applied once on the next boot; the ledger keeps the old name): statements are
-- `CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS` and
-- `ALTER TABLE accounts ADD COLUMN IF NOT EXISTS active_industry_sources` — the column and both
-- tables exist in production, so the file is a no-op there. It carries no INSERT/UPDATE/DELETE.
--
-- Industry Data Sources (Satellite integration)
CREATE TABLE IF NOT EXISTS industry_data_sources (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    industry_slug TEXT NOT NULL,
    source_name TEXT NOT NULL,
    source_type TEXT NOT NULL DEFAULT 'api' CHECK (source_type IN ('api','webhook','rss','scraper')),
    endpoint TEXT,
    refresh_cadence TEXT DEFAULT 'daily' CHECK (refresh_cadence IN ('realtime','hourly','daily','weekly','monthly')),
    credit_cost INTEGER DEFAULT 1,
    config JSONB DEFAULT '{}',
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(industry_slug, source_name)
);

-- Widget mappings: which widgets use which data sources
CREATE TABLE IF NOT EXISTS industry_widget_sources (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    industry_slug TEXT NOT NULL,
    widget_key TEXT NOT NULL,
    source_id UUID REFERENCES industry_data_sources(id) ON DELETE CASCADE,
    display_order INTEGER DEFAULT 0,
    is_active BOOLEAN DEFAULT true,
    UNIQUE(industry_slug, widget_key, source_id)
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_ids_industry ON industry_data_sources(industry_slug);
CREATE INDEX IF NOT EXISTS idx_iws_industry ON industry_widget_sources(industry_slug);

-- Account-level source activation (admin toggles per tenant)
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS active_industry_sources JSONB DEFAULT '[]';
