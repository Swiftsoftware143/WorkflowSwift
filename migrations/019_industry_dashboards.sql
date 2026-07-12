-- 019: Multi-Industry Dashboard System
-- Adds industry selection to tenants, seeds all industry categories,

-- Add industry_slug to tenants
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS industry_slug VARCHAR(100) DEFAULT 'site-flipping';

-- Seed all template categories for the 13 industries
INSERT INTO template_categories (id, slug, name, description, sort_order, is_active)
VALUES
    (gen_random_uuid(), 'sales-lead-gen', 'Sales & Lead Gen', 'Lead capture, nurturing, and sales pipeline automation for real estate, insurance, auto, solar, and more', 0, true),
    (gen_random_uuid(), 'service-businesses', 'Service Businesses', 'Estimate, schedule, invoice, and rebooking workflows for contractors, cleaners, landscapers, photographers', 1, true),
    (gen_random_uuid(), 'recruitment-staffing', 'Recruitment & Staffing', 'Resume screening, interview coordination, placement, and gig economy worker management', 2, true),
    (gen_random_uuid(), 'marketing-agencies', 'Marketing Agencies', 'Content calendars, client onboarding, ad campaign management, and reporting workflows', 3, true),
    (gen_random_uuid(), 'professional-services', 'Professional Services', 'Tax prep, legal intake, consulting engagement, and billing workflows for accountants, lawyers, consultants', 4, true),
    (gen_random_uuid(), 'ecommerce-retail', 'E-Commerce & Retail', 'Order fulfillment, supplier management, dropshipping, and inventory tracking workflows', 5, true),
    (gen_random_uuid(), 'healthcare-wellness', 'Healthcare & Wellness', 'Patient intake, appointment scheduling, treatment planning, and fitness client management', 6, true),
    (gen_random_uuid(), 'construction-development', 'Construction & Development', 'Permit management, subcontractor bidding, property maintenance, and development tracking', 7, true),
    (gen_random_uuid(), 'grant-funding', 'Grant & Funding', 'Grant writing, research, submission tracking, and fundraising donor management', 8, true),
    (gen_random_uuid(), 'education-training', 'Education & Training', 'Course creation, student onboarding, enrollment, and certificate management', 9, true),
    (gen_random_uuid(), 'publishing-media', 'Publishing & Media', 'Content approval pipelines, newsletter curation, editorial calendars, and publishing workflows', 10, true),
    (gen_random_uuid(), 'site-flipping', 'Site & Software Flipping', 'Website and software project tracking, marketplace listings, sales tracking, and TinyBrander funnel management', 11, true)
ON CONFLICT (slug) DO UPDATE SET is_active = true, sort_order = EXCLUDED.sort_order;

-- Update existing government-contracting category
UPDATE template_categories SET is_active = true WHERE slug = 'government-contracting';

-- Add default industry-specific dashboard widgets for site-flipping
INSERT INTO dashboard_widgets (id, dashboard_id, widget_type, title, config, position)
SELECT
    gen_random_uuid(),
    d.id,
    'stat-counter',
    'Projects Pipeline',
    '{"metric_key": "projects_pipeline", "subtitle": "In Development / Listed / Sold / Running"}',
    '{"row": 0, "col": 0, "width": 3, "height": 1}'
FROM dashboards d
WHERE EXISTS (SELECT 1 FROM tenants t WHERE t.id = d.tenant_id AND t.industry_slug = 'site-flipping')
AND NOT EXISTS (SELECT 1 FROM dashboard_widgets w WHERE w.dashboard_id = d.id AND w.title = 'Projects Pipeline');
