-- Migration 069: make the plan mirror columns agree with what the GATE resolves.
--
-- AF-5 (kanban t_d08c1146). David's decision on the record (2026-09-23): "the top tier plan gets
-- everything", and where a feature is stored in TWO places the two must be made to agree.
--
-- A plan's feature is stored twice: the `plan_tiers.features` JSONB key the admin panel writes,
-- and a legacy dedicated column under a DIFFERENT name. The gate resolves them in one order
-- (src/features.rs::aliases + resolve_plan_flag + resolve_limit):
--
--     1. features -> 'api_access'          (exact registry key)
--     2. features -> 'has_api_access'      (the bare alias)
--     3. plan_tiers.has_api_access         (the dedicated column -- reached only if 1 and 2 are null)
--     4. feature_limits.limit_value        (legacy per-plan override; 0 disables a boolean)
--
-- so a column that disagrees is a second, silently-diverging source of truth: the gate reads the
-- JSONB and the admin panel (`GET /api/v1/admin/plans`) printed the column as fact.
--
-- MEASURED 2026-09-30 on the live database (all four tiers):
--     has_api_access     = false on ALL FOUR   while  features->>'api_access' = f/t/t/t
--     can_deploy_n8n     = false on ALL FOUR   while  features->>'n8n_deploy' = f/t/t/t
--     can_export / max_workflows / max_users / retention_days agreed
-- i.e. Starter, Professional and Enterprise displayed "no API access / no n8n deploy" on a panel
-- the gate contradicted, and `free` advertised `max_api_keys: 1` while denying `api_access`.
--
-- This file repairs the COLUMN side (so the gate's own fallback path and every reader of the
-- column agree with the JSONB), DERIVED FROM THE DATA -- there is no per-tier list here, so a tier
-- added later is covered. Idempotent: re-running every statement is a no-op.
--
-- The panel side is fixed in code at the same time: `admin_list_plans` now reports the value the
-- gate RESOLVES (src/handlers/admin_settings_handler.rs), so a stale column can never make the
-- panel lie again -- but the column must still be truthful for the gate's own step 3.

-- 1. api_access (JSONB) -> has_api_access (column)
--    Type coercion mirrors the gate: bool, number (!= 0), or the string words it accepts.
UPDATE plan_tiers SET has_api_access = (
    CASE jsonb_typeof(features -> 'api_access')
        WHEN 'boolean' THEN (features ->> 'api_access')::boolean
        WHEN 'number'  THEN ((features ->> 'api_access')::numeric <> 0)
        WHEN 'string'  THEN lower(btrim(features ->> 'api_access'))
                              IN ('true', 'yes', 'on', '1', 'enabled')
        ELSE has_api_access
    END)
WHERE jsonb_typeof(features -> 'api_access') IN ('boolean', 'number', 'string');

-- 2. n8n_deploy (JSONB) -> can_deploy_n8n (column). Same pair shape, same measured drift.
UPDATE plan_tiers SET can_deploy_n8n = (
    CASE jsonb_typeof(features -> 'n8n_deploy')
        WHEN 'boolean' THEN (features ->> 'n8n_deploy')::boolean
        WHEN 'number'  THEN ((features ->> 'n8n_deploy')::numeric <> 0)
        WHEN 'string'  THEN lower(btrim(features ->> 'n8n_deploy'))
                              IN ('true', 'yes', 'on', '1', 'enabled')
        ELSE can_deploy_n8n
    END)
WHERE jsonb_typeof(features -> 'n8n_deploy') IN ('boolean', 'number', 'string');

-- 3. csv_export (JSONB) -> can_export (column). Already agreed live; included so no pair is left
--    out of the repair and a re-run stays honest about the whole class.
UPDATE plan_tiers SET can_export = (
    CASE jsonb_typeof(features -> 'csv_export')
        WHEN 'boolean' THEN (features ->> 'csv_export')::boolean
        WHEN 'number'  THEN ((features ->> 'csv_export')::numeric <> 0)
        WHEN 'string'  THEN lower(btrim(features ->> 'csv_export'))
                              IN ('true', 'yes', 'on', '1', 'enabled')
        ELSE can_export
    END)
WHERE jsonb_typeof(features -> 'csv_export') IN ('boolean', 'number', 'string');

-- 4. A tier that DENIES `api_access` must not advertise `max_api_keys` > 0.
--    `free` advertised 1 API key while `POST /api/v1/api-keys` answered 402 for every account on
--    it -- the UI lied about a limit the plan refuses (the follow-on t_3a50417c left open).
--    `0` is the module's OWN documented encoding for "not included in this plan" (see
--    src/features.rs: "`0` means \"not included in this plan\"") -- deliberately NOT deleting the
--    key, because an ABSENT limit resolves to `None` => allow, i.e. silent-unlimited the moment
--    `api_access` were ever granted on that tier. The rule is derived per tier and re-runnable.
UPDATE plan_tiers SET features = COALESCE(features, '{}'::jsonb)
                              || jsonb_build_object('max_api_keys', 0)
WHERE jsonb_typeof(features -> 'api_access') IN ('boolean', 'number', 'string')
  AND (CASE jsonb_typeof(features -> 'api_access')
           WHEN 'boolean' THEN (features ->> 'api_access')::boolean
           WHEN 'number'  THEN ((features ->> 'api_access')::numeric <> 0)
           ELSE lower(btrim(features ->> 'api_access'))
                    IN ('true', 'yes', 'on', '1', 'enabled')
       END) IS FALSE
  AND jsonb_typeof(features -> 'max_api_keys') = 'number'
  AND ((features ->> 'max_api_keys')::numeric <> 0);
