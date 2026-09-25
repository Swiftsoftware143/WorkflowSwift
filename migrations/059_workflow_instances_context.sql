-- 059_workflow_instances_context.sql
-- `workflow_instances.context` — the JSON payload that caused an instance to be created
-- (kanban t_b9c74751).
--
-- schedule_dashboard_data / push_dashboard_data builds a trigger payload
-- (`{data, source: "dashboard_trigger", metric_key}`) and inserts it with the instance
-- (src/handlers/dashboard_handler.rs); the column did not exist, so the INSERT was a guaranteed
-- ERROR 42703 and the dashboard-trigger feature never created a single instance. `result` is not a
-- substitute: it is written by the completion path (handlers/instance_handler.rs) and holds the
-- instance's OUTPUT, while `context` holds its INPUT.
--
-- Additive and idempotent, nullable — the capture/manual paths (handlers/incoming_handler.rs,
-- execution.rs::create_instance) legitimately create instances with no context.
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS context jsonb;

COMMENT ON COLUMN workflow_instances.context IS
    'Trigger payload that created this instance, e.g. {data, source, metric_key} for dashboard triggers (migrations/059).';
