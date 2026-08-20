-- 042_onbrand_tables.sql
-- On-brand tables for WorkflowSwift (per David: drop CRM bloat, keep leads + surfaces).
-- 1. surfaces — David's specified feature: admin CRUD; users tag/filter workflows by surface.
-- 2. leads — the workflow INPUT (a lead captured -> a workflow runs on it).
-- 3. Add surface_id to workflows + workflow_templates (completes migration 027, which
--    previously failed because the surfaces table did not exist).

-- ============ SURFACES ============
CREATE TABLE IF NOT EXISTS surfaces (
    id          UUID PRIMARY KEY,
    aid         UUID NOT NULL,
    name        TEXT NOT NULL,
    slug        TEXT NOT NULL,
    description TEXT,
    is_active   BOOLEAN DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_surfaces_aid_slug ON surfaces(aid, slug);

-- ============ LEADS (workflow inputs) ============
CREATE TABLE IF NOT EXISTS leads (
    id          UUID PRIMARY KEY,
    aid         UUID NOT NULL,
    name        TEXT NOT NULL,
    email       TEXT,
    phone       TEXT,
    company     TEXT,
    status      TEXT DEFAULT 'new',      -- new | contacted | qualified | lost
    source      TEXT,                    -- e.g. website | form | referral | import
    surface_id  UUID REFERENCES surfaces(id) ON DELETE SET NULL,
    workflow_id UUID,                    -- the workflow this lead feeds into (UUID, no FK to avoid migration coupling)
    notes       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_leads_aid ON leads(aid);
CREATE INDEX IF NOT EXISTS idx_leads_status ON leads(status);

-- ============ COMPLETE migration 027: add surface_id to workflows + templates ============
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL;
ALTER TABLE workflow_templates ADD COLUMN IF NOT EXISTS surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_workflows_surface ON workflows(surface_id);
CREATE INDEX IF NOT EXISTS idx_workflow_templates_surface ON workflow_templates(surface_id);
