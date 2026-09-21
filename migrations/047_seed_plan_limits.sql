-- 047_seed_plan_limits.sql
-- Canonical per-plan limit + flag keys, so that what the admin Plans UI writes and what
-- the API enforces are the SAME keys (features JSONB -> "max_*"/flag names).
-- Merge (||) so existing keys (users/workflows/instances/support/sla) are preserved.
-- Idempotent: re-running rewrites the same values.

UPDATE plan_tiers SET
  max_workflows  = CASE lower(slug) WHEN 'free' THEN 3 WHEN 'starter' THEN 15 ELSE -1 END,
  max_users      = CASE lower(slug) WHEN 'free' THEN 2 WHEN 'starter' THEN 10 WHEN 'professional' THEN 25 ELSE -1 END,
  retention_days = CASE lower(slug) WHEN 'free' THEN 30 WHEN 'starter' THEN 30 WHEN 'professional' THEN 90 ELSE 365 END,
  features = COALESCE(features, '{}'::jsonb) || CASE lower(slug)
    WHEN 'free' THEN '{"max_workflows":3,"max_users":2,"max_templates":3,"max_instances":10,"max_automations":1,"max_integrations":2,"max_api_keys":1,"max_clients":5,"max_portfolio":0,"max_tags":10,"max_industries":1,"retention_days":30,"n8n_deploy":false,"api_access":false,"csv_export":true,"webhook_export":true,"custom_branding":false,"priority_support":false,"dedicated_support":false,"sla_guarantee":false,"audit_logs":false,"custom_reports":false,"google_sheets":false}'::jsonb
    WHEN 'starter' THEN '{"max_workflows":15,"max_users":10,"max_templates":10,"max_instances":100,"max_automations":3,"max_integrations":5,"max_api_keys":3,"max_clients":25,"max_portfolio":3,"max_tags":25,"max_industries":3,"retention_days":30,"n8n_deploy":true,"api_access":true,"csv_export":true,"webhook_export":true,"custom_branding":true,"priority_support":false,"dedicated_support":false,"sla_guarantee":false,"audit_logs":false,"custom_reports":false,"google_sheets":true}'::jsonb
    WHEN 'professional' THEN '{"max_workflows":-1,"max_users":25,"max_templates":25,"max_instances":1000,"max_automations":10,"max_integrations":10,"max_api_keys":10,"max_clients":100,"max_portfolio":10,"max_tags":100,"max_industries":3,"retention_days":90,"n8n_deploy":true,"api_access":true,"csv_export":true,"webhook_export":true,"custom_branding":true,"priority_support":true,"dedicated_support":false,"sla_guarantee":false,"audit_logs":true,"custom_reports":true,"google_sheets":true}'::jsonb
    WHEN 'enterprise' THEN '{"max_workflows":-1,"max_users":-1,"max_templates":-1,"max_instances":-1,"max_automations":-1,"max_integrations":-1,"max_api_keys":-1,"max_clients":-1,"max_portfolio":-1,"max_tags":-1,"max_industries":-1,"retention_days":365,"n8n_deploy":true,"api_access":true,"csv_export":true,"webhook_export":true,"custom_branding":true,"priority_support":true,"dedicated_support":true,"sla_guarantee":true,"audit_logs":true,"custom_reports":true,"google_sheets":true}'::jsonb
    ELSE '{}'::jsonb
  END
WHERE lower(slug) IN ('free', 'starter', 'professional', 'enterprise');
