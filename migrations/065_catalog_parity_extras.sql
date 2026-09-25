-- 065_catalog_parity_extras.sql
-- WorkflowSwift: the two objects a from-zero build has and production does not — needed for exact
-- catalog parity once 064 has removed production's four post-034 duplicates.
-- Decision card t_fd27c832 (flagged there as "not worth a card of their own": both are additive,
-- both are what the shipped DDL declares, neither changes a row).
--
--   1. workflow_templates_surface_id_fkey.  042a declares the column WITH its FK
--      (`ADD COLUMN IF NOT EXISTS surface_id UUID REFERENCES surfaces(id) ON DELETE SET NULL`), so
--      every install built from this repository carries it; production's column was added out of
--      band WITHOUT it (production's `workflows.surface_id` DOES carry the equivalent
--      `workflows_surface_id_fkey` — measured — so this is an inconsistency inside production).
--      Measured before adding: 0 of 77 workflow_templates rows have a non-NULL surface_id, i.e.
--      the column is empty and the constraint validates on an empty set.
--      The ADD is guarded on "no orphan rows" rather than assumed: an FK is the one object class
--      where a bad row would abort the boot, and a boot must never be taken down by a data
--      condition this file can simply report. If orphans ever exist the file says so and leaves the
--      database bootable; the parity harness will then flag the difference instead of hiding it.
--   2. idx_available_providers_category.  036 creates it (`CREATE INDEX IF NOT EXISTS ... ON
--      available_providers(category)`). 036 is the file production never got to run in full, so
--      live has the column and none of the index — a pure performance/parity gap, no semantics.
--
-- LIVE SAFETY
--   Both statements are idempotent (`IF NOT EXISTS` / a pg_constraint existence check) and are
--   no-ops on a from-zero build, where 042a and 036 already created them. No BEGIN/COMMIT — the
--   runner wraps each file in one transaction and no other file here declares its own.

DO $$
DECLARE orphans bigint;
BEGIN
    IF to_regclass('public.workflow_templates') IS NOT NULL
       AND to_regclass('public.surfaces') IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM pg_constraint
                       WHERE conname = 'workflow_templates_surface_id_fkey'
                         AND conrelid = 'public.workflow_templates'::regclass) THEN
        SELECT count(*) INTO orphans
          FROM workflow_templates t
         WHERE t.surface_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM surfaces s WHERE s.id = t.surface_id);
        IF orphans = 0 THEN
            ALTER TABLE workflow_templates
              ADD CONSTRAINT workflow_templates_surface_id_fkey
              FOREIGN KEY (surface_id) REFERENCES surfaces(id) ON DELETE SET NULL;
            RAISE NOTICE '065: added workflow_templates_surface_id_fkey (0 orphan rows)';
        ELSE
            RAISE NOTICE '065: SKIPPED workflow_templates_surface_id_fkey — % orphan surface_id row(s)', orphans;
        END IF;
    END IF;
END $$;

DO $$
BEGIN
    IF to_regclass('public.available_providers') IS NOT NULL THEN
        CREATE INDEX IF NOT EXISTS idx_available_providers_category ON available_providers(category);
    END IF;
END $$;
