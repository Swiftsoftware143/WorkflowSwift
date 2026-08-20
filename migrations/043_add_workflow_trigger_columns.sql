-- 043_add_workflow_trigger_columns.sql
-- Fix schema drift: the Workflow model + create_workflow handler reference
-- trigger_type and trigger_config columns on workflows, but they were never
-- added by any migration. create_workflow INSERT was failing with
-- 'column "trigger_type" of relation "workflows" does not exist' (500).
--
-- Idempotent: safe to re-run (ADD COLUMN IF NOT EXISTS).
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS trigger_type VARCHAR(64) DEFAULT 'manual';
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS trigger_config JSONB DEFAULT '{}'::jsonb;
