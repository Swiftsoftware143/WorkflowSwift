-- 088 - the four platform blueprints that carried NO steps now carry their own steps
-- (kanban t_6c589b6b)
--
-- migrations/013_seed_data.sql seeded FIVE public platform blueprints
-- (`workflow_templates.aid IS NULL`, `is_public = true`) and wrote steps for exactly one of them
-- ("Seed steps for Government Contracting template" - a single DO block). The other four were left
-- as empty shells, and an empty shell cannot be installed:
--
--   POST /templates/{id}/install -> 400 {"message":"Template has no steps. Cannot install an empty
--                                        template."}
--
-- Measured live 2026-10-14 against this app's own API (127.0.0.1:8085), by name:
--
--   Client Onboarding                 steps=0   -> 400
--   Lead Generation                   steps=0   -> 400
--   Newsletter Campaign               steps=0   -> 400
--   Project Delivery                  steps=0   -> 400
--   Government Contracting Lifecycle  steps=10  -> 201
--
-- So the tenant console's Install button on four of the five gallery rows (app.workflowswift.com,
-- www-app/index.html::TemplatesView) could never succeed, and no tenant could repair the rows:
-- those are the PLATFORM rows, `update_template`/`delete_template` scope by `aid = $2` and answer
-- 404 on them, and no route writes `workflow_template_steps` for an existing template.
--
-- The steps below are not invented: each blueprint is the app's own canonical workflow, and each
-- file exists in n8n-templates/ under this same name (client-onboarding.json, lead-generation.json,
-- newsletter-auto-pilot.json, project-delivery.json). They are expressed in the step types this
-- app's engine can actually run, following the shape the Government Contracting row already
-- established (migration 073's own rationale):
--
--   * position 0 is a `data-card` - `assert_template_opens_with_data_card` refuses an install whose
--     template opens with anything else, and the tenant picks the dashboard widget for it in the
--     Builder (docs/user-guide.md, Templates);
--   * the human activities are `manual` - the run parks at the step and the console renders
--     Approve / Reject in the instance's run history;
--   * the two blueprints whose canonical graph waits on a clock carry a `delay` step with an
--     explicit `duration_seconds` (the key `crate::execution::delay_duration_ms` reads).
--
-- Deliberately NO `notify` and no `ai-action` step: both are sender/provider-backed, so
-- `assert_template_step_runnable` refuses such a step at install unless the INSTALLING tenant has
-- that channel configured. A platform blueprint must install for every tenant, so it may only carry
-- step types that need nothing from the tenant.
--
-- Guarded and idempotent. A file that raises is FATAL to the boot on a fresh database (src/db.rs),
-- so an absent table is a NOTICE and RETURN, never an exception. Each block inserts only when that
-- template still carries NO steps, so re-running the file is a no-op and a tenant's own edits to a
-- re-seeded row are never overwritten. The executable step types below are pinned to
-- src/execution.rs::EXECUTABLE_STEP_TYPES by the Rust test
-- `the_seeded_platform_blueprints_are_installable` (which also pins position 0 = data-card).
DO $mig$
DECLARE
    still_empty text;
BEGIN
    IF to_regclass('public.workflow_templates') IS NULL
       OR to_regclass('public.workflow_template_steps') IS NULL THEN
        RAISE NOTICE '088: skipped - workflow_templates/workflow_template_steps absent on this database';
        RETURN;
    END IF;

    -- blueprint: Client Onboarding (operations)
    INSERT INTO workflow_template_steps (id, template_id, step_type, name, description, sort_order, config)
    SELECT uuid_generate_v4(), t.id, v.step_type, v.name, v.description, v.sort_order, v.config
      FROM (VALUES
        ('data-card', 'New client intake', 'Pick the dashboard widget this run opens with - the new-client numbers you track', 0, NULL::jsonb),
        ('manual', 'Review the intake', 'Check the enquiry arrived with everything you need to quote and deliver', 1, NULL::jsonb),
        ('manual', 'Collect the onboarding documents', 'Signed agreement, tax form and any compliance paperwork, all received', 2, NULL::jsonb),
        ('manual', 'Set up the client account', 'Create the client record, name the owner and set the delivery start date', 3, NULL::jsonb),
        ('delay', 'Wait a week after kickoff', 'Then check the first week went well', 4, '{"duration_seconds": 604800}'::jsonb),
        ('manual', 'Hand over to delivery', 'Onboarding is done - the delivery owner takes the account from here', 5, NULL::jsonb)
      ) AS v(step_type, name, description, sort_order, config)
     CROSS JOIN (SELECT id FROM workflow_templates
                  WHERE name = 'Client Onboarding' AND category = 'operations') t
     WHERE NOT EXISTS (SELECT 1 FROM workflow_template_steps s WHERE s.template_id = t.id);

    -- blueprint: Lead Generation (marketing-agency)
    INSERT INTO workflow_template_steps (id, template_id, step_type, name, description, sort_order, config)
    SELECT uuid_generate_v4(), t.id, v.step_type, v.name, v.description, v.sort_order, v.config
      FROM (VALUES
        ('data-card', 'New leads this run', 'Pick the dashboard widget holding the leads this run works', 0, NULL::jsonb),
        ('manual', 'Qualify the lead', 'Check the lead matches the clients you want, and note where it came from', 1, NULL::jsonb),
        ('delay', 'Wait a day before the first contact', 'Give the lead a day before outreach', 2, '{"duration_seconds": 86400}'::jsonb),
        ('manual', 'Make the first contact', 'Send the first outreach and log what went out and when', 3, NULL::jsonb),
        ('delay', 'Wait three days for a reply', 'Then follow up if nothing came back', 4, '{"duration_seconds": 259200}'::jsonb),
        ('manual', 'Follow up', 'Second touch - a call, or a short reminder', 5, NULL::jsonb),
        ('manual', 'Move the lead into the pipeline', 'Put the lead at the right stage and book the next step', 6, NULL::jsonb)
      ) AS v(step_type, name, description, sort_order, config)
     CROSS JOIN (SELECT id FROM workflow_templates
                  WHERE name = 'Lead Generation' AND category = 'marketing-agency') t
     WHERE NOT EXISTS (SELECT 1 FROM workflow_template_steps s WHERE s.template_id = t.id);

    -- blueprint: Newsletter Campaign (marketing-agency)
    INSERT INTO workflow_template_steps (id, template_id, step_type, name, description, sort_order, config)
    SELECT uuid_generate_v4(), t.id, v.step_type, v.name, v.description, v.sort_order, v.config
      FROM (VALUES
        ('data-card', 'Subscriber numbers', 'Pick the dashboard widget holding your list size and engagement', 0, NULL::jsonb),
        ('manual', 'Plan the issue', 'Choose the stories for this issue and the send date', 1, NULL::jsonb),
        ('manual', 'Write and review the issue', 'Draft the newsletter and get it signed off', 2, NULL::jsonb),
        ('manual', 'Prepare the subscriber list', 'Add and remove subscribers and confirm who gets this issue', 3, NULL::jsonb),
        ('manual', 'Send the issue', 'Send from your own email tool, then note the send time here', 4, NULL::jsonb),
        ('delay', 'Wait three days for engagement', 'Then look at what the issue did', 5, '{"duration_seconds": 259200}'::jsonb),
        ('manual', 'Review the results', 'Note opens, clicks and unsubscribes and decide the next issue from them', 6, NULL::jsonb)
      ) AS v(step_type, name, description, sort_order, config)
     CROSS JOIN (SELECT id FROM workflow_templates
                  WHERE name = 'Newsletter Campaign' AND category = 'marketing-agency') t
     WHERE NOT EXISTS (SELECT 1 FROM workflow_template_steps s WHERE s.template_id = t.id);

    -- blueprint: Project Delivery (operations)
    INSERT INTO workflow_template_steps (id, template_id, step_type, name, description, sort_order, config)
    SELECT uuid_generate_v4(), t.id, v.step_type, v.name, v.description, v.sort_order, v.config
      FROM (VALUES
        ('data-card', 'Project board', 'Pick the dashboard widget this run opens with', 0, NULL::jsonb),
        ('manual', 'Kickoff', 'Confirm scope, dates and who owns what with the client', 1, NULL::jsonb),
        ('manual', 'Plan the work', 'Break the project into tasks and set the milestones', 2, NULL::jsonb),
        ('delay', 'Wait for the first milestone', 'Then check the milestone landed', 3, '{"duration_seconds": 604800}'::jsonb),
        ('manual', 'Check progress', 'Review what is done, what slipped and anything blocking', 4, NULL::jsonb),
        ('manual', 'Deliver', 'Hand over the finished work and record the client sign-off', 5, NULL::jsonb),
        ('manual', 'Close out', 'Capture the results, ask for the testimonial and hand the account to support', 6, NULL::jsonb)
      ) AS v(step_type, name, description, sort_order, config)
     CROSS JOIN (SELECT id FROM workflow_templates
                  WHERE name = 'Project Delivery' AND category = 'operations') t
     WHERE NOT EXISTS (SELECT 1 FROM workflow_template_steps s WHERE s.template_id = t.id);

    SELECT string_agg(t.name, ', ' ORDER BY t.name) INTO still_empty
      FROM workflow_templates t
     WHERE t.aid IS NULL
       AND NOT EXISTS (SELECT 1 FROM workflow_template_steps s WHERE s.template_id = t.id);
    RAISE NOTICE '088: platform blueprints carrying no steps after this migration: %',
                 coalesce(still_empty, '(none)');
    RAISE NOTICE '088: platform blueprint steps now stored: %',
                 (SELECT count(*) FROM workflow_template_steps s
                    JOIN workflow_templates t ON t.id = s.template_id
                   WHERE t.aid IS NULL);
END $mig$;
