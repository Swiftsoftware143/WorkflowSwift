-- 034: Rename tenants table and all tenant_id foreign key columns to aid
-- This migration renames the `tenants` table to `accounts` and updates
-- all foreign key references from `tenant_id` to `aid` across the database.

-- 1. Rename the core table
ALTER TABLE IF EXISTS tenants RENAME TO accounts;
ALTER TABLE IF EXISTS accounts RENAME COLUMN slug TO account_slug;
ALTER INDEX IF EXISTS idx_tenants_slug RENAME TO idx_accounts_slug;
ALTER INDEX IF EXISTS idx_tenants_is_active RENAME TO idx_accounts_is_active;

-- 2. Rename tenant_id columns in all referencing tables
-- Users
ALTER TABLE users RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_users_tenant RENAME TO idx_users_aid;

-- Clients
ALTER TABLE clients RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_clients_tenant RENAME TO idx_clients_aid;

-- Workflows
ALTER TABLE workflows RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_workflows_tenant RENAME TO idx_workflows_aid;

-- Workflow instances
ALTER TABLE workflow_instances RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_workflow_instances_tenant RENAME TO idx_workflow_instances_aid;

-- Dashboards
ALTER TABLE dashboards RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_dashboards_tenant RENAME TO idx_dashboards_aid;

-- Dashboard data
ALTER TABLE dashboard_data RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_dashboard_data_tenant RENAME TO idx_dashboard_data_aid;

-- Automations
ALTER TABLE automations RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_automations_tenant RENAME TO idx_automations_aid;

-- Credit transactions
ALTER TABLE credit_transactions RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_credit_transactions_tenant RENAME TO idx_credit_transactions_aid;

-- Tags
ALTER TABLE tags RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_tags_tenant RENAME TO idx_tags_aid;

-- Audit logs
ALTER TABLE audit_logs RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_audit_logs_tenant RENAME TO idx_audit_logs_aid;

-- API keys
ALTER TABLE api_keys RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_api_keys_tenant RENAME TO idx_api_keys_aid;

-- Portfolio companies
ALTER TABLE portfolio_companies RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_portfolio_companies_tenant RENAME TO idx_portfolio_companies_aid;

-- Integration targets
ALTER TABLE integration_targets RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_integration_targets_tenant RENAME TO idx_integration_targets_aid;

-- Brand monitors
ALTER TABLE brand_monitors RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_brand_monitors_tenant_id RENAME TO idx_brand_monitors_aid;

-- Extension ingest log
ALTER TABLE extension_ingest_log RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_extension_ingest_log_tenant_id RENAME TO idx_extension_ingest_log_aid;

-- Competitors
ALTER TABLE competitors RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_competitors_tenant_id RENAME TO idx_competitors_aid;

-- Prospecting leads
ALTER TABLE prospecting_leads RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_prospecting_leads_tenant_id RENAME TO idx_prospecting_leads_aid;

-- User integrations
ALTER TABLE user_integrations RENAME COLUMN tenant_id TO aid;
ALTER INDEX idx_user_integrations_tenant RENAME TO idx_user_integrations_aid;

-- Provider keys
ALTER TABLE provider_keys RENAME COLUMN tenant_id TO aid;
ALTER INDEX provider_keys_tenant_id_key RENAME TO provider_keys_aid_key;
ALTER INDEX idx_provider_keys_tenant_provider RENAME TO idx_provider_keys_aid_provider;

-- Dashboard data sources (from dashboard tabs migration)
ALTER TABLE dashboard_data_sources RENAME COLUMN tenant_id TO aid;
ALTER INDEX IF EXISTS idx_dashboard_sources_tenant RENAME TO idx_dashboard_sources_aid;

-- Brand monitor items
ALTER TABLE brand_monitor_items RENAME COLUMN tenant_id TO aid;
ALTER INDEX IF EXISTS idx_brand_monitor_tenant RENAME TO idx_brand_monitor_aid;

-- Competitor watch items
ALTER TABLE competitor_watch_items RENAME COLUMN tenant_id TO aid;

-- Prospecting items
ALTER TABLE prospecting_items RENAME COLUMN tenant_id TO aid;

-- Dashboard tab config
ALTER TABLE dashboard_tab_config RENAME COLUMN tenant_id TO aid;

-- Dashboard workflow links
ALTER TABLE dashboard_workflow_links RENAME COLUMN tenant_id TO aid;

-- 3. Rename tenant-specific tables
ALTER TABLE IF EXISTS tenant_plans RENAME TO account_plans;
ALTER TABLE account_plans RENAME COLUMN tenant_id TO aid;
ALTER INDEX IF EXISTS idx_tenant_plans_tenant RENAME TO idx_account_plans_aid;
ALTER INDEX IF EXISTS idx_tenant_plans_status RENAME TO idx_account_plans_status;

ALTER TABLE IF EXISTS tenant_industries RENAME TO account_industries;
ALTER TABLE account_industries RENAME COLUMN tenant_id TO aid;
ALTER INDEX IF EXISTS idx_tenant_industries_tenant RENAME TO idx_account_industries_aid;

-- Note: actual table name was tenant_n8n_config, not n8n_tenant_config
ALTER TABLE IF EXISTS tenant_n8n_config RENAME TO n8n_account_config;
ALTER TABLE IF EXISTS n8n_tenant_config RENAME TO n8n_account_config;
ALTER TABLE n8n_account_config RENAME COLUMN tenant_id TO aid;

-- 4. Workflow templates table
ALTER TABLE workflow_templates RENAME COLUMN tenant_id TO aid;

-- 5. Invoices table
ALTER TABLE invoices RENAME COLUMN tenant_id TO aid;

-- 6. Update user unique constraint
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_tenant_id_email_key;
ALTER TABLE users ADD CONSTRAINT users_aid_email_key UNIQUE (aid, email);

-- 7. Update for other tables with unique constraints on (tenant_id, ...)
ALTER TABLE tags DROP CONSTRAINT IF EXISTS tags_tenant_id_name_key;
ALTER TABLE tags ADD CONSTRAINT tags_aid_name_key UNIQUE (aid, name);

ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_tenant_id_provider_key;
ALTER TABLE provider_keys ADD CONSTRAINT provider_keys_aid_provider_key UNIQUE (aid, provider);

ALTER TABLE n8n_account_config DROP CONSTRAINT IF EXISTS n8n_tenant_config_tenant_id_key;
ALTER TABLE n8n_account_config ADD CONSTRAINT n8n_account_config_aid_key UNIQUE (aid);

ALTER TABLE account_industries DROP CONSTRAINT IF EXISTS tenant_industries_tenant_id_industry_slug_key;
ALTER TABLE account_industries ADD CONSTRAINT account_industries_aid_industry_slug_key UNIQUE (aid, industry_slug);

-- 8. Update references in other migration-dependent tables that have tenant_id
ALTER TABLE user_api_keys RENAME COLUMN tenant_id TO aid;

-- 9. Notify
NOTIFY pgrst, 'reload schema';
