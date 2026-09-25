-- 057_account_plans_one_active_row.sql
-- Enforce "at most one active account_plans row per account", the invariant two call sites had
-- already assumed but nothing enforced (kanban t_3914ee20, 2026-09-25).
--
-- Why it matters: the entitlement resolver `features.rs::resolve_plan_id` takes the NEWEST
-- `status = 'active'` row for an account and every entitlement check goes through it, so a second
-- active row is a silent mis-entitlement rather than an error. Two defects had already been
-- written against a uniqueness this table never had: `admin_create_account` bound
-- `ON CONFLICT (aid, plan_id)` and the deleted `plan_handler::admin_assign_plan` bound
-- `ON CONFLICT (aid)` - both are guaranteed Postgres errors on a table whose only unique guard is
-- the primary key on id, and both were replaced by a plain INSERT.
--
-- The live table before this file (measured 2026-09-25):
--   tenant_plans_pkey PRIMARY KEY btree (id)
--   idx_account_plans_aid    btree (aid)      <- non-unique
--   idx_account_plans_status btree (status)   <- non-unique
--
-- Step 1 aligns the legacy `is_active` boolean with `status`. They are two flags for one fact and
-- nothing in the crate ever wrote `is_active`, so a superseded row kept `is_active = true` and the
-- one reader that filtered on the flag saw two live rows.
UPDATE account_plans
   SET is_active = (status = 'active')
 WHERE is_active IS DISTINCT FROM (status = 'active');

-- Step 2 is the de-dup pass that has to run BEFORE the index, so the index can be created on any
-- host. Measured before this migration: 0 accounts had 2+ active rows and account_plans held 11
-- rows for 22 accounts, so this statement is a no-op today - it exists so a host whose history
-- differs cannot wedge the boot (a failed file is not recorded and is retried every start).
UPDATE account_plans
   SET status = 'superseded', is_active = false
 WHERE id IN (
   SELECT id FROM (
     SELECT id,
            row_number() OVER (PARTITION BY aid
                               ORDER BY started_at DESC, created_at DESC, id) AS rn
       FROM account_plans
      WHERE status = 'active'
   ) ranked
   WHERE rn > 1
 );

-- Step 3 enforces it. A PARTIAL unique index: the invariant is free for superseded rows. Note this
-- is an index, not a table constraint, so a bare `ON CONFLICT (aid)` still does NOT resolve against
-- it - the conflict target has to repeat the predicate, i.e.
-- `ON CONFLICT (aid) WHERE status = 'active'`. No code in this crate upserts this table, and
-- `admin_settings_handler::admin_assign_plan` supersedes then plain-INSERTs in one transaction, so
-- nothing needs to.
CREATE UNIQUE INDEX IF NOT EXISTS account_plans_one_active_per_aid
    ON account_plans (aid)
 WHERE status = 'active';
