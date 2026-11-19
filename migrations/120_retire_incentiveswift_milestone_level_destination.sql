-- 120_retire_incentiveswift_milestone_level_destination.sql
-- Kanban t_c1e9ec0c.
--
-- WHAT WAS WRONG
--   The catalogue offered "Trigger Milestone" a destination type `milestone_level`, which
--   WorkflowSwift resolved by fetching `{base}/api/milestones`. IncentiveSwift has no such route:
--   milestones are per-campaign (`GET /api/v1/campaigns/:slug/milestones`, measured 404 on the
--   un-scoped path). The value fetch has no campaign in scope, so the picker could never be filled
--   from the app and only ever showed a static Bronze/Silver/Gold list that exists nowhere.
--
-- WHY RETIRE RATHER THAN REPOINT
--   There is no campaign-independent milestone list to point at. Choosing a milestone requires
--   choosing the campaign first; when that cascade exists it belongs as a second level under a
--   campaign, not as a destination type that claims to list something globally.
--
-- SAFETY
--   No workflow step and no row outside this catalogue references the type (checked with
--   `workflow_steps.config LIKE '%milestone_level%'` -> 0 rows on the live database), so this
--   removes a dead door rather than work in progress. The action keeps its `campaign` destination.
DELETE FROM integration_destinations
 WHERE provider = 'incentiveswift'
   AND action_key = 'trigger_milestone'
   AND destination_type = 'milestone_level';
