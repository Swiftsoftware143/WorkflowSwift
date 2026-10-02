-- 075_retire_unbacked_plan_flags.sql
--
-- kanban t_413b4aab — WorkflowSwift's six remaining ungated boolean plan flags.
--
-- MEASURED 2026-10-02 against the running app (sha 76932ef) and the live DB: six boolean keys in
-- plan_tiers.features are read by NO gate. A consumer census of src/ (every key outside
-- src/features.rs) returns 0 hits for all six:
--
--   key                 on Free?  on starter+  mechanism measured
--   ------------------  --------  -----------  -------------------------------------------------
--   custom_branding     false     starter+     accounts.logo_url / branding_name / primary_color /
--                                              accent_color exist but no code path selects (row
--                                              serializer apart), writes or renders them; PUT
--                                              /accounts/{id} accepts no branding field; there is
--                                              no per-tenant public page to white-label at all
--   audit_logs          false     professional+ `audit_logs` table has 0 rows and 0 INSERTs; its
--                                              only reader (`GET /dashboard/activity`) was deleted
--                                              by kanban t_3a8ccd2a
--   custom_reports      false     professional+ no report table, handler, route or console surface
--   priority_support    false     professional+ SUPPORT-CONTRACT promise, not a code path
--   dedicated_support   false     enterprise    SUPPORT-CONTRACT promise, not a code path
--   sla_guarantee       false     enterprise    SUPPORT-CONTRACT promise, not a code path
--
-- VERDICT (one per key, decided by the mechanism measurement above):
--
--   * RETIRE custom_branding / audit_logs / custom_reports. They advertise a SOFTWARE capability
--     that does not exist anywhere in the crate; a plan that sells it is the defect, not a
--     feature. Removing them changes nothing a tenant receives: no console renders these keys,
--     `GET /api/v1/plans` is read by the tenant console for name+price only, and the marketing
--     site never names them.
--
--   * KEEP priority_support / dedicated_support / sla_guarantee. These are human promises
--     ("Priority email and chat support", "Dedicated account manager", "Service level agreement
--     guarantee") whose mechanism is the support process, not a code path -- exactly the class
--     the card says is a PRICING/support-contract decision that belongs to David. They stay in
--     plan_tiers.features and plan_feature_definitions so the plan editor keeps recording which
--     tier promises them, and they are now named explicitly in docs/admin-guide.md
--     (section "Support-tier promises") so the next "a flag no gate reads" census does not
--     re-flag them.
--
-- This is the same verdict shape kanban t_1aa78926 shipped for csv_export / webhook_export /
-- google_sheets (migration 074): sell-or-retire each claim on a mechanism you can point at.
--
-- REVERSAL (one batch, if the branding/reports feature is ever built):
--   UPDATE plan_tiers SET features = features
--     || CASE lower(slug)
--          WHEN 'free'         THEN '{"custom_branding":false,"audit_logs":false,"custom_reports":false}'::jsonb
--          WHEN 'starter'      THEN '{"custom_branding":true ,"audit_logs":false,"custom_reports":false}'::jsonb
--          WHEN 'professional' THEN '{"custom_branding":true ,"audit_logs":true ,"custom_reports":true }'::jsonb
--          WHEN 'enterprise'   THEN '{"custom_branding":true ,"audit_logs":true ,"custom_reports":true }'::jsonb
--          ELSE '{}'::jsonb END
--   WHERE lower(slug) IN ('free','starter','professional','enterprise');
--   INSERT INTO plan_feature_definitions (key,label,description,value_type,default_value,unit,category,sort_order)
--   VALUES ('custom_branding','Custom Branding','White-label branding options','boolean','false',NULL,'access',14),
--          ('audit_logs','Audit Logs','Access to audit log history','boolean','false',NULL,'access',18),
--          ('custom_reports','Custom Reports','Custom report builder access','boolean','false',NULL,'access',19)
--   ON CONFLICT (key) DO NOTHING;

-- 1. The three retired keys leave every plan's features JSONB.
--    Idempotent: `- key` on a map that no longer has the key is a no-op.
UPDATE plan_tiers
   SET features = COALESCE(features, '{}'::jsonb) - 'custom_branding' - 'audit_logs' - 'custom_reports'
 WHERE features ?| array['custom_branding', 'audit_logs', 'custom_reports'];

-- 2. ...and the plan editor stops offering them. DELETE (not is_visible=false): the console
--    renders an invisible definition row anyway, which is how a retired knob survives a deploy.
DELETE FROM plan_feature_definitions
 WHERE key IN ('custom_branding', 'audit_logs', 'custom_reports');

-- 3. Nothing else to move: `feature_limits` has 0 rows for these three keys (measured), so no
--    legacy per-plan override can resurrect them.
--
-- NOT touched, deliberately:
--   * plan_tiers.features->>'priority_support' / 'dedicated_support' / 'sla_guarantee' and their
--     three plan_feature_definitions rows (kept: support-contract promises, documented).
--   * plan_tiers.can_deploy_n8n / has_api_access and the n8n_deploy / api_access keys -- gated.
--   * the accounts branding columns (logo_url / branding_name / primary_color / accent_color):
--     dead weight measured here (no writer, no renderer), but a schema change is not needed to
--     stop advertising the plan flag; removing them is a separate card if the feature is built.
