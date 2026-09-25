-- 063: the columns PRODUCTION has that no migration creates — measured, not invented.
-- Card t_4ebd6f98. Runs LAST (after every file that creates a table), so every target exists.
--
-- WHY
--   A from-zero build now applies all 71 files with rc=0 (000_baseline_live_schema.sql, the five
--   renamed files and the neutralised 019/021), and a diff of the built catalog against production
--   still showed 167 columns on 61 tables that production has and the build did not. They are one
--   uniform out-of-band drift: `plan_id uuid` on 59 tables, `sort_order integer` on 51 and
--   `is_active boolean` on 38, plus a handful of single columns (`accounts.hexomatic_key`,
--   `accounts.slug`, `account_industries.icon`, `integration_targets.allowed_domains`,
--   `admin_settings.value_type` ...). They were added to the live database by the admin UI /
--   hand-run SQL, never by a migration, which is why a fresh install could boot yet still 500 on
--   routes that name them — e.g. src/handlers/account_handler.rs writes
--   `UPDATE accounts SET hexomatic_key = $1` and `SET slug = $1`, src/features.rs reads
--   `SELECT plan_id FROM accounts`, and src/handlers/integration_target_handler.rs selects
--   `allowed_domains`, `daily_limit` and `sort_order` from integration_targets.
--
-- SHAPE
--   Every statement is `ADD COLUMN IF NOT EXISTS` with the type, nullability and DEFAULT copied
--   from the production catalog (`format_type` + `pg_get_expr(pg_attrdef)`), so the fresh build
--   reaches production's shape instead of a second, invented one. The column list was generated
--   from the measured diff (`/opt/swift/audits/t_4ebd6f98/live.columns` minus `zero.columns`),
--   not hand-picked.
--
-- PRODUCTION SAFETY
--   This file is applied once on live at the next boot (its name is new) and every statement is a
--   no-op there: all 167 columns already exist, so `IF NOT EXISTS` skips each one. It writes no
--   rows, drops nothing, and cannot change a type, a default or a constraint of an existing column.
--
-- NOT INCLUDED, DELIBERATELY
--   The 4 relations production still carries that 034_rename_tenant_to_account.sql renamed away
--   (tenants, tenant_plans, tenant_industries, tenant_n8n_config) and their 43 columns, indexes and
--   constraints: they hold 0 rows, no shipped code reads them (src/handlers/admin_settings_handler.rs
--   :1422 already documents `the legacy tenants table has 0 rows`), and re-creating them would put
--   the pre-034 duplicates back into a fresh install. Reported as residual drift in the card's
--   REPORT.md instead.

-- account_industries
ALTER TABLE account_industries ADD COLUMN IF NOT EXISTS icon text;
ALTER TABLE account_industries ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE account_industries ADD COLUMN IF NOT EXISTS slug text;
ALTER TABLE account_industries ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- account_renditions
ALTER TABLE account_renditions ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE account_renditions ADD COLUMN IF NOT EXISTS plan_id uuid;

-- accounts
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS hexomatic_key text;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS icon text;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS payment_provider text;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS slug text;
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- admin_settings
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS aid uuid;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS category text;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS created_at timestamp with time zone DEFAULT now();
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS default_value text;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS is_visible text;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS label text;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS unit text;
ALTER TABLE admin_settings ADD COLUMN IF NOT EXISTS value_type text;

-- agent_profiles
ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- agent_tickets
ALTER TABLE agent_tickets ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE agent_tickets ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE agent_tickets ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- api_keys
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- audit_logs
ALTER TABLE audit_logs ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE audit_logs ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE audit_logs ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- automation_runs
ALTER TABLE automation_runs ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE automation_runs ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE automation_runs ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- automations
ALTER TABLE automations ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE automations ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- brand_monitor_items
ALTER TABLE brand_monitor_items ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE brand_monitor_items ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- brand_monitor_results
ALTER TABLE brand_monitor_results ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE brand_monitor_results ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE brand_monitor_results ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- brand_monitors
ALTER TABLE brand_monitors ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE brand_monitors ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- checkout_sessions
ALTER TABLE checkout_sessions ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE checkout_sessions ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE checkout_sessions ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- client_contacts
ALTER TABLE client_contacts ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE client_contacts ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE client_contacts ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- clients
ALTER TABLE clients ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE clients ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- competitor_watch_items
ALTER TABLE competitor_watch_items ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE competitor_watch_items ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- competitor_watch_results
ALTER TABLE competitor_watch_results ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE competitor_watch_results ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE competitor_watch_results ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- competitors
ALTER TABLE competitors ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE competitors ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- compliance_checks
ALTER TABLE compliance_checks ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE compliance_checks ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE compliance_checks ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- compliance_items
ALTER TABLE compliance_items ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE compliance_items ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE compliance_items ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- credit_packages
ALTER TABLE credit_packages ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE credit_packages ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- credit_transactions
ALTER TABLE credit_transactions ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE credit_transactions ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE credit_transactions ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- dashboard_data
ALTER TABLE dashboard_data ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE dashboard_data ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE dashboard_data ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- dashboard_data_sources
ALTER TABLE dashboard_data_sources ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE dashboard_data_sources ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- dashboard_tab_config
ALTER TABLE dashboard_tab_config ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE dashboard_tab_config ADD COLUMN IF NOT EXISTS plan_id uuid;

-- dashboard_widgets
ALTER TABLE dashboard_widgets ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE dashboard_widgets ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE dashboard_widgets ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- dashboard_workflow_links
ALTER TABLE dashboard_workflow_links ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE dashboard_workflow_links ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE dashboard_workflow_links ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- dashboards
ALTER TABLE dashboards ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE dashboards ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE dashboards ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- extension_commands
ALTER TABLE extension_commands ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE extension_commands ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE extension_commands ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- extension_ingest_log
ALTER TABLE extension_ingest_log ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE extension_ingest_log ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE extension_ingest_log ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- industry_data_sources
ALTER TABLE industry_data_sources ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE industry_data_sources ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- industry_widget_sources
ALTER TABLE industry_widget_sources ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE industry_widget_sources ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- industry_widgets
ALTER TABLE industry_widgets ADD COLUMN IF NOT EXISTS plan_id uuid;

-- integration_destinations
ALTER TABLE integration_destinations ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE integration_destinations ADD COLUMN IF NOT EXISTS plan_id uuid;

-- integration_targets
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS allowed_domains text[] DEFAULT '{}'::text[];
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS daily_limit integer DEFAULT 1000;
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE integration_targets ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- invoices
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- n8n_account_config
ALTER TABLE n8n_account_config ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE n8n_account_config ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- password_resets
ALTER TABLE password_resets ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE password_resets ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE password_resets ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- payment_providers
ALTER TABLE payment_providers ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE payment_providers ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- payment_webhook_events
ALTER TABLE payment_webhook_events ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE payment_webhook_events ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE payment_webhook_events ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- plan_capabilities
ALTER TABLE plan_capabilities ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- plan_feature_definitions
ALTER TABLE plan_feature_definitions ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE plan_feature_definitions ADD COLUMN IF NOT EXISTS plan_id uuid;

-- portfolio_companies
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- prospecting_items
ALTER TABLE prospecting_items ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE prospecting_items ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- prospecting_leads
ALTER TABLE prospecting_leads ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE prospecting_leads ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE prospecting_leads ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- prospecting_results
ALTER TABLE prospecting_results ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE prospecting_results ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE prospecting_results ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- provider_keys
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS aid uuid;
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS icon text;
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS slug text;
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- tag_assignments
ALTER TABLE tag_assignments ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE tag_assignments ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE tag_assignments ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- tags
ALTER TABLE tags ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE tags ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE tags ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- ticket_steps
ALTER TABLE ticket_steps ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE ticket_steps ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE ticket_steps ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- user_integrations
ALTER TABLE user_integrations ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE user_integrations ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- users
ALTER TABLE users ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE users ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- workflow_execution_logs
ALTER TABLE workflow_execution_logs ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_execution_logs ADD COLUMN IF NOT EXISTS plan_id uuid;

-- workflow_instance_steps
ALTER TABLE workflow_instance_steps ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_instance_steps ADD COLUMN IF NOT EXISTS plan_id uuid;

-- workflow_instances
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- workflow_step_integrations
ALTER TABLE workflow_step_integrations ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_step_integrations ADD COLUMN IF NOT EXISTS plan_id uuid;

-- workflow_steps
ALTER TABLE workflow_steps ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_steps ADD COLUMN IF NOT EXISTS plan_id uuid;

-- workflow_template_steps
ALTER TABLE workflow_template_steps ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_template_steps ADD COLUMN IF NOT EXISTS plan_id uuid;

-- workflow_trigger_queue
ALTER TABLE workflow_trigger_queue ADD COLUMN IF NOT EXISTS is_active boolean DEFAULT true;
ALTER TABLE workflow_trigger_queue ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE workflow_trigger_queue ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;

-- workflows
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS plan_id uuid;
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS sort_order integer DEFAULT 0;
