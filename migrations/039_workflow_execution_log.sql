-- Migration 039: Workflow Execution Engine tables
-- Adds tables for local step execution, dashboard trigger queue, and execution logs

-- Table: workflow_execution_logs
-- Records every step execution attempt for auditing and debugging
CREATE TABLE IF NOT EXISTS workflow_execution_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id UUID NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    workflow_id UUID NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    step_id UUID REFERENCES workflow_steps(id) ON DELETE SET NULL,
    step_type VARCHAR(64) NOT NULL,
    step_name VARCHAR(255) NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',  -- pending, running, completed, failed, skipped
    provider VARCHAR(64),
    input_data JSONB DEFAULT '{}',
    output_data JSONB DEFAULT '{}',
    error_message TEXT,
    duration_ms INTEGER DEFAULT 0,
    started_at TIMESTAMPTZ DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_exec_logs_instance ON workflow_execution_logs(instance_id);
CREATE INDEX IF NOT EXISTS idx_exec_logs_workflow ON workflow_execution_logs(workflow_id);
CREATE INDEX IF NOT EXISTS idx_exec_logs_status ON workflow_execution_logs(status);

-- Table: workflow_trigger_queue
-- Queues workflow triggers from dashboard data pushes or webhooks
CREATE TABLE IF NOT EXISTS workflow_trigger_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_id UUID NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    aid UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    trigger_type VARCHAR(64) NOT NULL,  -- dashboard_data, webhook, scheduled
    trigger_source VARCHAR(255),         -- metric_key or webhook path
    payload JSONB DEFAULT '{}',
    status VARCHAR(32) NOT NULL DEFAULT 'pending',  -- pending, processing, completed, failed
    client_id UUID,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    processed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_trigger_queue_pending ON workflow_trigger_queue(status, created_at) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_trigger_queue_workflow ON workflow_trigger_queue(workflow_id);

-- Add trigger_source column to dashboard_data for workflow matching
ALTER TABLE dashboard_data ADD COLUMN IF NOT EXISTS trigger_source VARCHAR(64);
