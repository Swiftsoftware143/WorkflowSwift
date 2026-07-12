-- Add n8n_target column to workflows table (which n8n instance this workflow deploys to)
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS n8n_target VARCHAR(255) DEFAULT 'default';

-- Add n8n_execution_id to workflow_instances for callback tracing
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS n8n_execution_id VARCHAR(255);

-- New table for tenant-specific n8n configuration
CREATE TABLE IF NOT EXISTS tenant_n8n_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    n8n_url VARCHAR(512) NOT NULL DEFAULT 'http://user-n8n-webhook:5679',
    n8n_api_key VARCHAR(255) DEFAULT '',
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id)
);
