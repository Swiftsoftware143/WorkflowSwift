-- 000a: the industry vocabulary — the 6 industries this platform ships, for the two tables that
-- carry it. Card t_4ebd6f98. Runs second (after 000_baseline_live_schema.sql creates the tables,
-- before 031_multi_industry_dashboards.sql seeds plan_capabilities FROM template_categories).
--
-- WHY THIS IS A FILE AND NOT A COMMENT
--   `template_categories` IS the industry table: GET /api/v1/industries serves its rows as the
--   industry picker (src/handlers/industry_handler.rs:18), `plan_capabilities.industry_slug`,
--   `account_industries.industry_slug` and `workflow_templates.category` are all compared against
--   `template_categories.slug`, and the served admin guide names exactly these six
--   (docs/admin-guide.md:153). Every one of those rows was written OUT OF BAND in production
--   (created_at 2026-08-11 06:24:40 for template_categories, 06:25:30 for industries) — no
--   migration ever inserted them. A fresh install therefore booted to an EMPTY picker even after
--   every file applied cleanly, which is the second half of this card's acceptance.
--
--   migrations 019 and 021 used to INSERT a 13-row SECTOR vocabulary into this table (sales-lead-gen,
--   service-businesses, recruitment-staffing, marketing-agencies, professional-services,
--   ecommerce-retail, healthcare-wellness, construction-development, grant-funding,
--   education-training, publishing-media, site-flipping, government-contracting). Those INSERTs are
--   neutralised in place: a sector row served by GET /api/v1/industries is a bogus industry option,
--   and 061 (card t_9f7083b0) already decided that the INDUSTRY vocabulary wins — it renamed the
--   sector values still sitting in workflow_templates.category to the industry they belong to and
--   left the remainder documented as unreachable sector buckets.
--
-- VALUES
--   Copied byte-for-byte from the six rows production serves today: name, slug and `is_active`;
--   template_categories additionally carries icon '📁' and sort_order 0 (all six), `description` is
--   NULL on both tables, and `industries` carries nothing but name/slug/is_active. No pricing, no
--   operator-owned data: nothing here is a business decision, it is the vocabulary the routes and
--   the plan_capabilities seed read.
--
-- IDEMPOTENT, AND A NO-OP ON PRODUCTION
--   `INSERT ... SELECT ... WHERE NOT EXISTS (slug)` rather than ON CONFLICT: it survives any index
--   shape, and on the production database all six slugs exist, so this file writes ZERO rows and
--   cannot duplicate the six rows or resurrect a sector row. `created_at` is left to the column
--   default so the file never fights a re-run.

INSERT INTO template_categories (name, slug, icon, sort_order, is_active)
SELECT v.name, v.slug, v.icon, 0, true
FROM (VALUES
    ('Site Flipping',          'site-flipping',          '📁'),
    ('E-Commerce',             'e-commerce',             '📁'),
    ('SaaS',                   'saas',                   '📁'),
    ('Real Estate',            'real-estate',            '📁'),
    ('Government Contracting', 'government-contracting', '📁'),
    ('Marketing Agency',       'marketing-agency',       '📁')
) AS v(name, slug, icon)
WHERE NOT EXISTS (SELECT 1 FROM template_categories tc WHERE tc.slug = v.slug);

INSERT INTO industries (name, slug, is_active)
SELECT v.name, v.slug, true
FROM (VALUES
    ('Site Flipping',          'site-flipping'),
    ('E-Commerce',             'e-commerce'),
    ('SaaS',                   'saas'),
    ('Real Estate',            'real-estate'),
    ('Government Contracting', 'government-contracting'),
    ('Marketing Agency',       'marketing-agency')
) AS v(name, slug)
WHERE NOT EXISTS (SELECT 1 FROM industries i WHERE i.slug = v.slug);
