-- Per-step integration binding for workflow steps and templates
-- Users pick which integration target each step uses

-- Add integration_target_id to workflow_step config is already supported via JSONB, 
-- but add a dedicated column for fast lookups if needed
ALTER TABLE workflow_steps ADD COLUMN IF NOT EXISTS integration_target_id UUID REFERENCES integration_targets(id) ON DELETE SET NULL;
ALTER TABLE workflow_template_steps ADD COLUMN IF NOT EXISTS integration_target_id UUID REFERENCES integration_targets(id) ON DELETE SET NULL;

-- Add an endpoint config helper: stores API path/method for the step
ALTER TABLE workflow_steps ADD COLUMN IF NOT EXISTS api_path TEXT;
ALTER TABLE workflow_steps ADD COLUMN IF NOT EXISTS api_method TEXT DEFAULT 'POST';
ALTER TABLE workflow_template_steps ADD COLUMN IF NOT EXISTS api_path TEXT;
ALTER TABLE workflow_template_steps ADD COLUMN IF NOT EXISTS api_method TEXT DEFAULT 'POST';

-- Steps can use multiple integrations (fan-out)
CREATE TABLE IF NOT EXISTS workflow_step_integrations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    step_id UUID NOT NULL REFERENCES workflow_steps(id) ON DELETE CASCADE,
    template_step_id UUID REFERENCES workflow_template_steps(id) ON DELETE CASCADE,
    integration_target_id UUID NOT NULL REFERENCES integration_targets(id) ON DELETE CASCADE,
    payload_template JSONB DEFAULT '{}',   -- template for what to send
    sort_order INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_step_integrations_step ON workflow_step_integrations(step_id);
CREATE INDEX IF NOT EXISTS idx_step_integrations_template_step ON workflow_step_integrations(template_step_id);
