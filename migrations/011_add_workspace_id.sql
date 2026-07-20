-- Migration 011: Add workspace (portfolio_company_id) support
-- This scopes user data by workspace without creating new accounts.

ALTER TABLE workflows ADD COLUMN IF NOT EXISTS portfolio_company_id uuid REFERENCES portfolio_companies(id) ON DELETE SET NULL;
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS portfolio_company_id uuid REFERENCES portfolio_companies(id) ON DELETE SET NULL;
ALTER TABLE clients ADD COLUMN IF NOT EXISTS portfolio_company_id uuid REFERENCES portfolio_companies(id) ON DELETE SET NULL;
ALTER TABLE automations ADD COLUMN IF NOT EXISTS portfolio_company_id uuid REFERENCES portfolio_companies(id) ON DELETE SET NULL;
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS portfolio_company_id uuid REFERENCES portfolio_companies(id) ON DELETE SET NULL;

-- Indexes for filtered queries
CREATE INDEX IF NOT EXISTS idx_workflows_portfolio_company ON workflows(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_instances_portfolio_company ON workflow_instances(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_clients_portfolio_company ON clients(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_automations_portfolio_company ON automations(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_provider_keys_portfolio_company ON provider_keys(portfolio_company_id);

-- User-accessible integrations table (BYOK for power users, scoped per workspace)
-- This exists alongside provider_keys (which is admin/system-level)
ALTER TABLE user_integrations ADD COLUMN IF NOT EXISTS portfolio_company_id uuid REFERENCES portfolio_companies(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_user_integrations_portfolio_company ON user_integrations(portfolio_company_id);

-- Expose portfolio_companies to users as workspaces (currently admin-only in frontend)
-- No schema change needed -- it's already per-aid, just needs a user-facing endpoint
