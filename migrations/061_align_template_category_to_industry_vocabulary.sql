-- 061: align workflow_templates.category with the platform's industry vocabulary
-- Card t_9f7083b0. Data-only: no DDL, no Rust, no row created or deleted.
--
-- PROBLEM
--   GET /api/v1/templates?industry=<slug> joins `template_categories tc ON tc.slug = wt.category`
--   and filters on the requested slug, so the two string vocabularies have to be the same one.
--   `template_categories.slug` is this platform's industry vocabulary (its 6 slugs are exactly
--   `industries.slug`, and `GET /api/v1/industries` serves that table as the industry picker).
--   `workflow_templates.category` was still carrying the older *sector* vocabulary written by
--   migrations 019/021, so 4 of the 6 industries returned zero templates while two returned the
--   only values that happened to coincide (government-contracting, site-flipping).
--
-- DIRECTION (card acceptance 1): rename the DATA. No mapping table, no second derived copy.
--   Evidence that the industry vocabulary is the one that must win:
--     * `template_categories` IS the industry table here - src/handlers/industry_handler.rs:18
--       serves it as the industry list, so inserting a sector row would offer a bogus industry.
--     * `plan_capabilities` (plan -> industry access) holds the same 6 slugs for all 4 plans.
--     * `account_industries` holds them (14 live rows, all site-flipping).
--     * the served admin guide, docs/admin-guide.md:153: "The platform ships 6 industries:
--       site-flipping, e-commerce, saas, government-contracting, real-estate, marketing-agency".
--     * the served SPA binds a template to an industry with that same string:
--       `t.category === <industry slug>` (www-app/index.html:526) and renders t.category as the
--       industry chip (:539, :546).
--     * the migration-031 view v_plan_industry_templates already joins tc.slug = wt.category.
--
-- MAPPING RULE
--   A sector value is renamed to the industry whose business its own app-authored description
--   (migrations 019/021 - the files that created both vocabularies) names:
--     ecommerce-retail -> e-commerce        (021 "Ecommerce & Retail" vs industry "E-Commerce")
--     marketing        -> marketing-agency  (021 "Marketing Agencies ... ad campaign management")
--     sales-lead-gen   -> real-estate       (019 "lead capture, nurturing, and sales pipeline
--                                            automation for real estate, insurance, auto, solar")
--     government-contracting and site-flipping already equal the slug - nothing to rename.
--
-- WHAT IS DELIBERATELY NOT TOUCHED
--   Every remaining value names a business sector that is NOT one of the six industries this
--   platform ships: service-businesses, professional-services, healthcare-wellness,
--   construction-development, grant-funding, education-training, publishing-media,
--   recruitment-staffing, content-creation, operations, onboarding, general, General. No industry
--   can be derived for them without inventing one, so they stay sector buckets, exactly as
--   reachable as they were before this file ran: for no industry. The only industry left with no
--   public template afterwards is `saas`, which is a fact about the template library, not about
--   the route (no template in it denotes a SaaS business).
--
--   `category_id` carries the same drift: it still holds ids from the sector rows
--   `template_categories` had before it was reset to the industry list, so 59 rows point at no
--   row in any table. Nothing in src/ writes it (it is only echoed back at
--   src/handlers/template_handler.rs:509), so it is repointed at whatever `category` now names.
--
-- WHY THIS IS ONE GUARDED DO BLOCK
--   src/db.rs runs each file as a single batch over the simple query protocol and, by default,
--   makes an unrecorded failure fatal - so every statement below has to resolve on a database
--   where the tables exist, and must not 42P01 on one where they do not. The two tables are
--   therefore probed with to_regclass first and the statements run through EXECUTE (plain text,
--   no interpolation). On this platform's database the guard always passes.

DO $mig$
DECLARE
    cov       text;
    untouched text;
BEGIN
    IF to_regclass('public.workflow_templates') IS NULL
       OR to_regclass('public.template_categories') IS NULL THEN
        RAISE NOTICE '061: skipped - workflow_templates/template_categories absent on this database';
        RETURN;
    END IF;

    -- 1. The alignment.
    EXECUTE $u$UPDATE workflow_templates SET category = 'e-commerce'
              WHERE category = 'ecommerce-retail'$u$;
    EXECUTE $u$UPDATE workflow_templates SET category = 'marketing-agency'
              WHERE category = 'marketing'$u$;
    EXECUTE $u$UPDATE workflow_templates SET category = 'real-estate'
              WHERE category = 'sales-lead-gen'$u$;

    -- 2. Point category_id at the category row `category` now names (0 rows once aligned).
    EXECUTE $u$UPDATE workflow_templates wt
                 SET category_id = tc.id
                FROM template_categories tc
               WHERE tc.slug = wt.category
                 AND wt.category_id IS DISTINCT FROM tc.id$u$;

    -- 3. Report, do not fail: the post-state lands in the boot log as NOTICEs.
    EXECUTE $u$SELECT coalesce(string_agg(tc.slug || '=' || coalesce(w.n, 0), ', ' ORDER BY tc.slug),
                               '(no industry rows)')
                 FROM template_categories tc
                 LEFT JOIN (SELECT category, count(*) AS n
                              FROM workflow_templates
                             WHERE is_public
                             GROUP BY category) w ON w.category = tc.slug$u$
      INTO cov;

    EXECUTE $u$SELECT coalesce(string_agg(DISTINCT category, ', '), '(none)')
                 FROM workflow_templates
                WHERE category NOT IN (SELECT slug FROM template_categories)$u$
      INTO untouched;

    RAISE NOTICE '061: public templates reachable per industry: %', cov;
    RAISE NOTICE '061: category values outside the industry vocabulary (sector buckets, untouched): %', untouched;
END $mig$;
