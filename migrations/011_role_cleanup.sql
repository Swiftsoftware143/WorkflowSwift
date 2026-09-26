-- 011: role vocabulary and the singleton-admin index.
--
-- ============================ OWNER DECISION, TAKEN 2026-09-25 (card t_2255fdea) ============================
-- The role NORMALISATION this file used to perform is DELIBERATELY NOT APPLIED — not on production,
-- not on a fresh install. Its three UPDATEs are REMOVED (not merely guarded), so no future run of this
-- file, on any database, can demote an account or clear a super-admin flag.
--
-- DECISION: (a) leave production un-normalised. The app does not require the normalisation, and the
-- normalisation as written could not even be applied. What it did, and why each part is wrong here
-- (all measured 2026-09-25, evidence in /opt/swift/audits/t_2255fdea/):
--
--   1. `UPDATE users SET role='admin', perm_is_super_admin=true WHERE email='swiftsoftware143@yahoo.com'`
--      targets BY EMAIL, and that address owns TWO users rows in two different accounts
--      (users_aid_email_key is (aid,email): the rows sit in 'Probe Co' and 'SwiftSoftware'). Two rows
--      holding role='admin' violates idx_unique_admin below, so the whole file died with
--      `ERROR: duplicate key value violates unique constraint "idx_unique_admin"` — reproduced for real
--      on a RESTORED COPY of production (10-old011.txt, rc=1). Under src/db.rs an UNRECORDED failure is
--      FATAL ("Refusing to serve: N migration file(s)"), so this statement was live ammunition for a
--      boot refusal on any database whose ledger lost the row. It has never run anywhere: production's
--      ledger records the filename (the match is filename-only — src/db.rs keeps no checksum), and a
--      fresh install skipped the block because users.perm_is_super_admin does not exist until 038.
--   2. `UPDATE users SET role='user' WHERE role IN ('company_admin', 'staff', …)` would demote the four
--      live company_admin rows to 'user'. company_admin is a role this app MINTS and USES:
--      user_handler.rs TENANT_ROLES = ['team_member','company_admin'], is_tenant_admin() accepts it, and
--      the team screen renders it "Admin — full workspace access, can manage the team". Three of the
--      four rows are the sibling businesses' owner accounts (SwiftImpact, ZaarHub, Giraudy Capital); the
--      fourth is a leftover verify fixture. 'user' means ACCOUNT OWNER and set_user_role() refuses to
--      re-role an owner, so that UPDATE would relabel-then-freeze them for no functional gain.
--   3. `UPDATE users SET perm_is_super_admin=false WHERE email <> 'swiftsoftware143@yahoo.com'` would
--      clear the flag on anyone but David — today that is nobody (both flagged rows ARE his).
--
-- WHY NO NORMALISATION IS NEEDED: NO LIVE ROUTE AUTHORIZES ON role='admin'. The only five statements in
-- the crate that name that literal (plan_handler.rs:291,331,350,418,440) belong to handlers routes.rs
-- never mounts; every live admin gate reads `perm_is_super_admin`, which is already true on both of
-- David's rows. Reproduce in one line: `grep -n 'plan_handler::' src/routes.rs` lists only list_plans /
-- create_plan / get_plan_capabilities, and the served admin console's plan screens call /admin/plans
-- (admin_settings_handler, guarded by perm_is_super_admin). Full census: 00-role-census.txt.
--
-- REVERSAL (worth one migration if the owner ever asks for a singleton role='admin'): it must be exactly
-- ONE row, chosen explicitly, with idx_unique_admin left intact —
--     UPDATE users SET role = 'admin' WHERE id = '<the one chosen row>';
-- Do NOT restore the email-based form above; that is the 23505. Nothing else changes if you do this:
-- 'admin' is accepted by is_tenant_admin() alongside the other tenant roles.
-- ==========================================================================================================
--
-- HISTORY (kept for the next reader)
--   * 038_user_permissions_and_roles.sql creates users.perm_is_super_admin and grants it to
--     swiftsoftware143@yahoo.com; 040_seed_admin_user.sql seeds admin@swiftsoftware.com with
--     role='admin' + the flag. A fresh install therefore HAS a role='admin' row and production does
--     not — a difference in SEED ROWS, not in schema, and it is carded with the other seed gaps
--     (t_82d61045) rather than patched here. Production's admin identity is David's own address.
--   * This file is GUARDED/IN PLACE and never RENAMED. A rename would drop it out of production's
--     ledger and make it run at the next boot — and until 2026-09-25 that meant demoting four accounts.
--     Two statements below still carry guards for their own reasons (`admin_settings` is created by
--     037, i.e. 26 files later).
--   * `UPDATE users SET role='team_member' WHERE role='team_member'` (a value reassigned to itself,
--     no-op on any data) was removed with the three UPDATEs; the schema has no triggers, so it could
--     never have done anything.

-- Only one row may hold role='admin'. KEPT — it is part of the schema a fresh build must reproduce, and
-- it is the guard rail that made UPDATE #1 above fail loudly instead of silently minting a second admin.
CREATE UNIQUE INDEX IF NOT EXISTS idx_unique_admin
    ON users (role) WHERE role = 'admin';

-- Seed the email settings row. GUARDED (card t_4ebd6f98): `admin_settings` is created by
-- 037_admin_settings_and_retention.sql, 26 files after this one, so on a from-zero database an
-- unguarded INSERT raised `relation "admin_settings" does not exist` and refused the boot. It is not
-- moved or renamed because production has both files recorded and skipping is free there; the same
-- `email` row is seeded by 037 instead (the file that owns admin_settings), so a fresh install still
-- ends up with it.
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
