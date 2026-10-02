-- 072_retire_unread_step_columns.sql
-- Retire migration 018's remaining unread columns (kanban t_c603a937). Measured on LIVE
-- 2026-10-02 before the drop:
--   * `grep -rn 'api_path\|api_method' src/` = 0 hits — no reader AND no writer, in any table.
--   * `workflow_steps`: api_path IS NOT NULL = 0 rows; api_method <> 'POST' = 0 rows (the DEFAULT is
--     the only value any row carries); integration_target_id IS NOT NULL = 1 row (STAYS).
--   * `workflow_template_steps` (10 rows, govcon-lifecycle): api_path = 0, api_method <> 'POST' = 0,
--     integration_target_id = 0.
--   * Neither the steps API nor the template API can carry them: the step INSERT/UPDATE touch
--     (id, workflow_id, step_type, name, description, sort_order, config); template steps the same;
--     and the template -> workflow install path copies exactly those columns too
--     (src/handlers/template_handler.rs). So the twin columns could never acquire a value.
--
-- What STAYS and why:
--   * `workflow_steps.integration_target_id` — read by the executor (src/execution.rs, arm
--     "integration" | "integration_dispatch") and by POST /api/v1/integration-dispatch. See
--     migrations/067_drop_orphaned_step_integrations.sql and kanban t_97a0bd3f.
--   * `workflow_template_steps.integration_target_id` is DROPPED: it is the unused twin — the binding
--     is operator-provisioned directly on the workflow step (t_97a0bd3f) and template install does not
--     copy it, so it is dead weight with 0 rows and 0 writers.
--
-- DROP COLUMN IF EXISTS is idempotent, and a from-zero replay stays consistent: 018 adds these
-- columns, this migration removes them, so replay == live. A column that no path reads and no path
-- writes is the same defect class as a write-only column: it invites a future writer to fill a slot
-- nothing consumes.
ALTER TABLE workflow_steps DROP COLUMN IF EXISTS api_path;
ALTER TABLE workflow_steps DROP COLUMN IF EXISTS api_method;
ALTER TABLE workflow_template_steps DROP COLUMN IF EXISTS api_path;
ALTER TABLE workflow_template_steps DROP COLUMN IF EXISTS api_method;
ALTER TABLE workflow_template_steps DROP COLUMN IF EXISTS integration_target_id;
