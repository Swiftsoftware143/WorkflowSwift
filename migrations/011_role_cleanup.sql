-- 011: Enforce three-role system: admin, user, team_member
-- Only David (swiftsoftware143@yahoo.com) can be admin
-- All existing non-admin roles get normalized
--
-- REVISED 2026-09-25 (card t_4ebd6f98). The three UPDATEs below name
-- `users.perm_is_super_admin`, a column that 038_user_permissions_and_roles.sql creates — 27 files
-- LATER than this one — so on a from-zero database this file died with
-- `ERROR: column "perm_is_super_admin" of relation "users" does not exist` and, per src/db.rs, an
-- unrecorded failure is FATAL: the service refused to boot.
--
-- WHY THIS FILE IS GUARDED IN PLACE AND NOT RENAMED to run after 038
--   Renaming is the repair used everywhere else in this card, and here it would be WRONG. A renamed
--   file is not in production's `_migrations` ledger, so it runs on the next boot — and these three
--   UPDATEs are DATA normalisation, not schema. Measured on production 2026-09-25 (read-only):
--     users: 7 x role='user', 4 x role='company_admin', 2 x role='super_admin', 13 rows total
--     4 rows would be rewritten to role='user' by statement 2 (company_admin -> user)
--     1 row would lose perm_is_super_admin by statement 3
--   i.e. renaming this file would silently demote four live accounts and clear one super-admin flag
--   at the next boot. That is not this card's decision to make, so the statements are wrapped in a
--   guard instead: on a fresh database the column does not exist yet and they are skipped (harmless —
--   `users` is empty at this point; the first account is seeded by 040_seed_admin_user.sql), and on
--   production this file is recorded and skipped as it always has been. Nothing in the shipped code
--   depends on the normalisation: production has run un-normalised since 2026-08-09.
--
-- WHAT IS *NOT* SKIPPED: `idx_unique_admin` and the admin_settings seed below do not touch the
-- missing column, so they still run on a fresh install — the partial unique index is part of the
-- schema a fresh build must reproduce.
--
-- OWNER DECISION (not this card's): whether production's 4 `company_admin` rows should be
-- normalised to `user` and whether its second `perm_is_super_admin` row should keep the flag.
-- Recorded in the card's REPORT.md as a follow-up for David; nothing was changed here.

DO $mig$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'users' AND column_name = 'perm_is_super_admin'
    ) THEN
        -- 1. Reset David's account to be the sole admin with super_admin flag
        UPDATE users
        SET role = 'admin', perm_is_super_admin = true
        WHERE email = 'swiftsoftware143@yahoo.com';

        -- 2. All other accounts with admin/company_admin/staff/etc -> normalize to user
        UPDATE users
        SET role = 'user'
        WHERE email <> 'swiftsoftware143@yahoo.com'
          AND role IN ('admin', 'company_admin', 'staff', 'company_owner', 'manager');

        -- 3. Anyone with role = 'user' and perm_is_super_admin = true -> only David keeps it
        UPDATE users
        SET perm_is_super_admin = false
        WHERE email <> 'swiftsoftware143@yahoo.com';
    ELSE
        RAISE NOTICE '011: skipped the role normalisation - users.perm_is_super_admin does not exist yet (created by 038_user_permissions_and_roles.sql)';
    END IF;
END $mig$;

-- 4. Add a unique constraint ensuring only one admin (David)
-- We'll enforce this at the application level + a partial unique index
-- This prevents any future INSERT/UPDATE from creating a second admin
CREATE UNIQUE INDEX IF NOT EXISTS idx_unique_admin
    ON users (role) WHERE role = 'admin';

-- 5. Ensure team_member role is lowercase and consistent
UPDATE users SET role = 'team_member' WHERE role = 'team_member';

-- 6. Add email_settings to admin_settings if not present
--    GUARDED (card t_4ebd6f98) for the same reason as 1-3: `admin_settings` is created by
--    037_admin_settings_and_retention.sql, 26 files after this one, so on a from-zero database this
--    INSERT raised `relation "admin_settings" does not exist` and refused the boot. It is not moved
--    or renamed because production has both files recorded and skipping is free there; the same
--    `email` row is seeded by 037 instead (the file that owns admin_settings), so a fresh install
--    still ends up with it.
DO $mig$
BEGIN
    IF to_regclass('public.admin_settings') IS NOT NULL THEN
        INSERT INTO admin_settings (key, value, description)
        SELECT 'email', '{
  "api_url": "",
  "api_key": "",
  "from_address": "swiftsoftware143@yahoo.com",
  "from_name": "WorkflowSwift",
  "provider": "smtp",
  "smtp_host": "",
  "smtp_port": 587,
  "smtp_username": "",
  "smtp_password": "",
  "smtp_use_tls": true
}'::jsonb, 'Email/SMTP configuration for sending transactional emails'
        WHERE NOT EXISTS (SELECT 1 FROM admin_settings WHERE key = 'email');
    ELSE
        RAISE NOTICE '011: skipped the email settings seed - admin_settings does not exist yet (created by 037_admin_settings_and_retention.sql, which seeds this row)';
    END IF;
END $mig$;
