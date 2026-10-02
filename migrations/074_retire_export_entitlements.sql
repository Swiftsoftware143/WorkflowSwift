-- 074_retire_export_entitlements.sql
--
-- Retire the three export / Sheets plan entitlements that sold nothing (kanban t_1aa78926).
--
-- MEASURED 2026-10-02 on the live DB, before this migration:
--   select slug, features->>'csv_export', features->>'webhook_export', features->>'google_sheets'
--   from plan_tiers;
--     free         | true | true | false
--     starter      | true | true | true
--     professional | true | true | true
--     enterprise   | true | true | true
--   plan_feature_definitions: 3 rows (csv_export, webhook_export, google_sheets, all 'integrations')
--   plan_tiers.can_export:    the dedicated mirror column (migrations/037 + 069), true on all 4 plans
--   feature_limits:           0 rows for any of the four keys
--   served payloads:          GET /api/v1/plans (tenant) and GET /api/v1/admin/plans (operator) each
--                             carried every one of these keys, 4x; GET /api/v1/admin/feature-definitions
--                             served all three definitions.
--
-- DECISION — arm RETIRE, one verdict per flag, each decided by a mechanism that does NOT exist:
--   csv_export     no CSV writer, no store and no download surface anywhere in the crate (the only
--                  Content-Disposition: attachment in the tree is the extension .zip download).
--                  Its one consumer-shaped surface was the Export STEP, retired in t_02519738 on the
--                  same measurement ("CSV has no store and no download surface").
--   webhook_export the app's only outbound is the tenant-URL http-request / integration step, which
--                  no plan flag gates; there is no export contract for a flag to gate.
--   google_sheets  no Google credential, OAuth flow, provider preset or destination row exists
--                  (integration_provider_presets: 8 presets, none Google; integration_destinations:
--                  19 live rows, none google_sheets).
--   Retiring rather than implementing is what keeps this out of a PRICING decision the card reserves
--   for David: all three are already `true` on every plan including Free, so a gate could never fire
--   without first deciding which tier LOSES the perk. Dropping them removes an advertisement that
--   delivers nothing and changes nothing any tenant can actually receive — the same verdict
--   t_ab963d11 reached for the sold `max_api_keys` plan row.
--
-- REVERSAL (one batch; the two stores this file writes):
--   ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS can_export BOOLEAN DEFAULT true;
--   UPDATE plan_tiers SET features = features
--       || '{"csv_export":true,"webhook_export":true,"google_sheets":true,"can_export":true}'::jsonb,
--     can_export = true;
--   INSERT INTO plan_feature_definitions
--     (key,label,description,value_type,default_value,unit,category,sort_order) VALUES
--     ('webhook_export','Webhook Exports','Export workflow data via webhooks','boolean','true',NULL,'integrations',20),
--     ('csv_export','CSV/Excel Export','Export data to CSV or Excel','boolean','true',NULL,'integrations',21),
--     ('google_sheets','Google Sheets Sync','Sync data to Google Sheets','boolean','false',NULL,'integrations',22);
--
-- Idempotent: every statement below is a no-op on a second run, and the runner re-runs any file
-- whose `_migrations` ledger row is missing (src/db.rs).

-- 1. the JSONB store the API resolves (and the served plan payload echoes wholesale).
--    `can_export` is the bare alias migrations/069 mirrored into the column; strip it too so no
--    input alias survives in the data.
UPDATE plan_tiers
   SET features = features - 'csv_export' - 'webhook_export' - 'google_sheets' - 'can_export'
 WHERE features ?| ARRAY['csv_export', 'webhook_export', 'google_sheets', 'can_export'];

-- 2. the definitions the plan editor renders ("these define what shows up in the plan editor",
--    migrations/037). Three live rows; the other 19 keys are untouched.
DELETE FROM plan_feature_definitions
 WHERE key IN ('csv_export', 'webhook_export', 'google_sheets');

-- 3. the dedicated mirror column. Nothing reads it once src/features.rs' alias and
--    admin_settings_handler's payload/binds lose the key, and leaving it would keep a sold flag
--    in a store the admin API can still echo.
ALTER TABLE plan_tiers DROP COLUMN IF EXISTS can_export;
