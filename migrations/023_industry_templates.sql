-- 023: Link existing workflow templates to appropriate industries
-- Maps templates to industries based on their category slug

-- Ensure id column has a default UUID generator
ALTER TABLE industry_templates ALTER COLUMN id SET DEFAULT gen_random_uuid();

-- idempotent: clean existing links first so we can re-run safely
DELETE FROM industry_templates;

-- Templates in government-contracting category -> government-contracting industry
INSERT INTO industry_templates (industry_slug, template_id)
SELECT 'government-contracting', wt.id
FROM workflow_templates wt
JOIN template_categories tc ON tc.id = wt.category_id
WHERE tc.slug = 'government-contracting'
ON CONFLICT (industry_slug, template_id) DO NOTHING;

-- Templates in marketing category -> marketing-agencies industry
INSERT INTO industry_templates (industry_slug, template_id)
SELECT 'marketing-agencies', wt.id
FROM workflow_templates wt
JOIN template_categories tc ON tc.id = wt.category_id
WHERE tc.slug = 'marketing'
ON CONFLICT (industry_slug, template_id) DO NOTHING;

-- Templates in operations category -> service-businesses industry
INSERT INTO industry_templates (industry_slug, template_id)
SELECT 'service-businesses', wt.id
FROM workflow_templates wt
JOIN template_categories tc ON tc.id = wt.category_id
WHERE tc.slug = 'operations'
ON CONFLICT (industry_slug, template_id) DO NOTHING;

-- Templates in onboarding category -> professional-services industry
INSERT INTO industry_templates (industry_slug, template_id)
SELECT 'professional-services', wt.id
FROM workflow_templates wt
JOIN template_categories tc ON tc.id = wt.category_id
WHERE tc.slug = 'onboarding'
ON CONFLICT (industry_slug, template_id) DO NOTHING;
