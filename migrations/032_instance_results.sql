-- Migration: 032_instance_results
-- Adds result storage column to workflow_instances for n8n callback data

ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS result JSONB DEFAULT NULL;
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS error_text TEXT DEFAULT NULL;
