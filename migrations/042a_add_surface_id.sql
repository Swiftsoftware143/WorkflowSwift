-- 042a: workflows/workflow_templates.surface_id — RENAMED from `027_add_surface_id.sql`
-- (card t_4ebd6f98).
--
-- WHY THE RENAME
--   The file FKs `surfaces(id)`, and `surfaces` is created by 042_onbrand_tables.sql. As `027_*` it
--   ran 15 files before the table existed and a from-zero build died with
--   `ERROR: relation "surfaces" does not exist`. `042a` runs immediately after 042 and before 043.
--
-- GUARDS ADDED
--   `ADD COLUMN IF NOT EXISTS` (both ALTERs) and `CREATE INDEX IF NOT EXISTS` (both indexes): the
--   file is now applied once on the production database too (the ledger keeps the old name), where
--   the column and its index already exist — without the guards this renamed file would refuse
--   the boot with `column "surface_id" ... already exists`.
--
-- 027: Add surface_id to workflows and workflow_templates (on-brand surface capture)

ALTER TABLE workflows ADD COLUMN IF NOT EXISTS surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL;
ALTER TABLE workflow_templates ADD COLUMN IF NOT EXISTS surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_workflows_surface ON workflows(surface_id);
CREATE INDEX IF NOT EXISTS idx_workflow_templates_surface ON workflow_templates(surface_id);
