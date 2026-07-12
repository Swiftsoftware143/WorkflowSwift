-- 031: Multi-Industry Dashboard System
-- Enables multiple industry dashboards per tenant with plan-based limits
-- Also creates a `template_capabilities` table linking plan->industry->templates

-- 1. Create tenant_industries join table (replaces single industry_slug on tenants)
-- This is the core of multi-industry: a tenant can have 1+ industry dashboards
CREATE TABLE IF NOT EXISTS tenant_industries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    industry_slug VARCHAR(100) NOT NULL,
    dashboard_id UUID REFERENCES dashboards(id) ON DELETE SET NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, industry_slug)
);

CREATE INDEX IF NOT EXISTS idx_tenant_industries_tenant ON tenant_industries(tenant_id);
CREATE INDEX IF NOT EXISTS idx_tenant_industries_slug ON tenant_industries(industry_slug);

-- 2. Migrate existing data: copy each tenant's current industry_slug to tenant_industries
INSERT INTO tenant_industries (tenant_id, industry_slug, is_active)
SELECT id, COALESCE(industry_slug, 'site-flipping'), true
FROM tenants
WHERE industry_slug IS NOT NULL
ON CONFLICT (tenant_id, industry_slug) DO NOTHING;

-- 3. Link existing dashboards to tenant_industries
UPDATE tenant_industries ti
SET dashboard_id = d.id
FROM dashboards d
WHERE d.tenant_id = ti.tenant_id
  AND d.name = ti.industry_slug || ' Dashboard'
  AND ti.dashboard_id IS NULL;

-- 4. Add plan_capabilities table — controls which templates each plan gets access to
-- This replaces the ad-hoc feature_limits approach for template access
CREATE TABLE IF NOT EXISTS plan_capabilities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID NOT NULL REFERENCES plan_tiers(id) ON DELETE CASCADE,
    industry_slug VARCHAR(100) NOT NULL,
    max_industries INT NOT NULL DEFAULT 1,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (plan_id, industry_slug)
);

CREATE INDEX IF NOT EXISTS idx_plan_capabilities_plan ON plan_capabilities(plan_id);

-- 5. Seed plan capabilities — defines what industries a plan can access + how many dashboards
INSERT INTO plan_capabilities (plan_id, industry_slug, max_industries)
SELECT p.id, tc.slug, 
    CASE 
        WHEN p.slug = 'free' THEN 1
        WHEN p.slug = 'starter' THEN 3
        WHEN p.slug = 'professional' THEN 3
        WHEN p.slug = 'enterprise' THEN -1  -- -1 = unlimited
        ELSE 1
    END
FROM plan_tiers p, template_categories tc
WHERE p.slug IN ('free', 'starter', 'professional', 'enterprise')
  AND tc.is_active = true
ON CONFLICT (plan_id, industry_slug) DO UPDATE SET max_industries = EXCLUDED.max_industries;

-- 6. Add max_industries to feature_limits for plans that don't have it yet
INSERT INTO feature_limits (plan_id, feature_key, limit_value)
SELECT p.id, 'max_industries',
    CASE 
        WHEN p.slug = 'free' THEN 1
        WHEN p.slug = 'starter' THEN 3
        WHEN p.slug = 'professional' THEN 3
        WHEN p.slug = 'enterprise' THEN -1
        ELSE 1
    END
FROM plan_tiers p
WHERE p.slug IN ('free', 'starter', 'professional', 'enterprise')
  AND NOT EXISTS (
    SELECT 1 FROM feature_limits fl 
    WHERE fl.plan_id = p.id AND fl.feature_key = 'max_industries'
  );

-- 7. Clean up old single-industry columns (keep for backward compat during transition)
-- The tenants.industry_slug column stays as the "primary/active" industry for now

-- 8. Create template_access view for efficient querying
CREATE OR REPLACE VIEW v_plan_industry_templates AS
SELECT 
    pt.id AS plan_id,
    pt.slug AS plan_slug,
    tc.slug AS industry_slug,
    tc.name AS industry_name,
    wt.id AS template_id,
    wt.name AS template_name,
    wt.category AS template_category
FROM plan_tiers pt
JOIN plan_capabilities pc ON pc.plan_id = pt.id AND pc.is_active = true
JOIN template_categories tc ON tc.slug = pc.industry_slug AND tc.is_active = true
JOIN workflow_templates wt ON wt.category = tc.slug
WHERE wt.is_public = true;
