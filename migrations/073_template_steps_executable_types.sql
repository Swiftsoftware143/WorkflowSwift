-- 073 - workflow_template_steps.step_type must hold an EXECUTABLE step type, never a stage name
-- (kanban t_27a15474)
--
-- migrations/013_seed_data.sql seeded the ten Government Contracting lifecycle stages straight into
-- `step_type` ('discovery', 'qualify', 'team', 'propose', 'submit', 'track', 'manage', 'intel',
-- 'outreach', 'dashboard') even though the human label already lives in the row's own `name` column
-- ('Discover', 'Qualify', ...). `POST /templates/{id}/install` copies `step_type` VERBATIM into
-- `workflow_steps`, and the engine has exactly one arm table (src/execution.rs
-- `EXECUTABLE_STEP_TYPES`) that no stage word is a member of - so every template install produced a
-- workflow in which EVERY step fell through the `_` arm. Since kanban t_fe60cdf5 that arm is honest
-- (status `skipped`, an `unexecutable` marker, the run's warnings[] names it) but the installed
-- workflow still ran nothing at all.
--
-- The stage words are the wrong vocabulary for `step_type` - they are lifecycle phases, not engine
-- operations - and none of them collides with a real step type, so the repair is a vocabulary remap
-- keyed on those exact literals. Re-running this file is a no-op:
--
--   * 'discovery' -> 'data-card'  step 1 of a workflow is a Data Card (create_workflow_step's
--                                 guardrail, docs/user-guide.md, the Builder picker's own hint)
--                                 and the Discover stage is this template's first row.
--   * the other nine -> 'manual'  the stage descriptions are human activities (evaluate
--                                 eligibility, assemble a team, prepare and submit a proposal,
--                                 manage the awarded contract, ...) and `manual` is the one arm
--                                 that carries a human gate: the engine parks the run at the step
--                                 and the console renders Approve/Reject.
--
-- Guarded, single-batch and idempotent. A file that raises is FATAL to the boot on a fresh database
-- (src/db.rs), so absent tables are a NOTICE and RETURN, never an exception. The remap is keyed on
-- the exact stage vocabulary, and any `step_type` left outside the executable vocabulary is
-- REPORTED, never rewritten. The executable list below mirrors
-- src/execution.rs::EXECUTABLE_STEP_TYPES and is pinned to it by the Rust test
-- `the_migration_vocabulary_matches_the_engine`.
DO $mig$
DECLARE
    moved    integer;
    leftover text;
BEGIN
    IF to_regclass('public.workflow_template_steps') IS NULL THEN
        RAISE NOTICE '073: skipped - workflow_template_steps absent on this database';
        RETURN;
    END IF;

    EXECUTE $u$
        UPDATE workflow_template_steps s
           SET step_type = m.mapped
          FROM (VALUES ('discovery', 'data-card'), ('qualify', 'manual'), ('team', 'manual'),
                       ('propose', 'manual'), ('submit', 'manual'), ('track', 'manual'),
                       ('manage', 'manual'), ('intel', 'manual'), ('outreach', 'manual'),
                       ('dashboard', 'manual')) AS m(stage, mapped)
         WHERE s.step_type = m.stage
    $u$;
    GET DIAGNOSTICS moved = ROW_COUNT;
    RAISE NOTICE '073: template steps remapped from stage names to executable types: %', moved;

    EXECUTE $u$
        SELECT coalesce(string_agg(DISTINCT step_type, ', '), '(none)')
          FROM workflow_template_steps
         WHERE step_type NOT IN (
                 'http-request', 'action', 'ai-action', 'data-card', 'data_card', 'notify',
                 'export', 'delay', 'wait', 'transform', 'code', 'fork', 'branch',
                 'render_video', 'render_media', 'render_image', 'render_audio', 'generate',
                 'format', 'design', 'publish', 'loop', 'condition', 'manual', 'webhook')
    $u$ INTO leftover;
    RAISE NOTICE '073: template steps left outside the executable vocabulary (untouched): %', leftover;
END $mig$;
