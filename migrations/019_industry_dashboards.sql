-- 019: Multi-Industry Dashboard System
-- Adds industry selection to tenants.
--
-- NEUTRALISED 2026-09-25 (card t_4ebd6f98): the two statements that wrote the SECTOR vocabulary
-- into `template_categories` are gone from this file. See the block below for why.
-- Everything else in this file is unchanged and still runs.

-- Add industry_slug to tenants
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS industry_slug VARCHAR(100) DEFAULT 'site-flipping';

-- NEUTRALISED: this file used to INSERT 12 *sector* categories into `template_categories`
-- (sales-lead-gen, service-businesses, recruitment-staffing, marketing-agencies,
-- professional-services, ecommerce-retail, healthcare-wellness, construction-development,
-- grant-funding, education-training, publishing-media, site-flipping) and then flip
-- `government-contracting` active.
--
--   * `template_categories` is THIS PLATFORM'S INDUSTRY TABLE, not a sector taxonomy:
--     GET /api/v1/industries serves its rows as the industry picker
--     (src/handlers/industry_handler.rs:18), `plan_capabilities.industry_slug`,
--     `account_industries.industry_slug` and the served SPA's `t.category === <slug>` all compare
--     against `template_categories.slug`, and its six slugs are exactly `industries.slug`. A
--     sector row is therefore served as a bogus industry option (four of them — 'marketing-agencies'
--     vs the industry 'marketing-agency' — do not even spell an industry).
--   * No migration created the table: on a from-zero build these statements raised 42P01 and, per
--     src/db.rs, an unrecorded failure is FATAL, so the service refused to boot. That is the defect
--     this card was filed for. The table now comes from 000_baseline_live_schema.sql and its six
--     industry rows from 000a_industry_vocabulary.sql.
--   * Its vocabulary had already been superseded by card t_9f7083b0 (migration 061), which decided
--     the INDUSTRY vocabulary wins and renamed the sector values that were still sitting in
--     `workflow_templates.category`.
--   * In production the file is recorded in `_migrations` (2026-08-09) and is skipped, so removing
--     these statements cannot change the live database; it only stops a fresh build from seeding 12
--     rows the live database does not have.

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
