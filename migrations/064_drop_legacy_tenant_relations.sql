-- 064_drop_legacy_tenant_relations.sql
-- WorkflowSwift: DROP the four relations production still carries that 034 RENAMED AWAY.
-- Decision card t_fd27c832 (the parity gap t_4ebd6f98 deliberately left open).
-- DECISION: DROP.  Production is the side carrying the duplicates; a from-zero build
-- (001 creates `tenants` -> 034 renames it to `accounts`, same for the three others) is the
-- CORRECT shape, and it is the shape every other database of this app already has.
--
-- WHY THESE FOUR ARE RESIDUE AND NOT PLANNED SCHEMA (evidence /opt/swift/audits/t_fd27c832/):
--   1. THEY WERE MADE AFTER 034, OUT OF BAND. The tables 034 renamed carry the ORIGINAL,
--      UN-SUFFIXED constraint names (`accounts.tenants_pkey`, `account_plans.tenant_plans_pkey`,
--      `account_industries.tenant_industries_pkey`, `n8n_account_config.tenant_n8n_config_pkey`).
--      These four carry the `..._pkey1` / `..._tenant_id_fkey1` / `..._tenant_id_key1` names
--      Postgres mints only when the un-suffixed name is ALREADY TAKEN — i.e. the pre-034 DDL
--      (001_create_tenants.sql, 010_create_plans.sql, 031_multi_industry_dashboards.sql,
--      033_n8n_tenant_config.sql) was re-applied by hand to an already-renamed database.  Nothing
--      in this repository creates them after 034, so no shipped code can have intended them back.
--   2. NO READER, EVER — AT HEAD.  No SQL statement in src/, www/, www-app/ or extensions/ names
--      any of the four (`grep -rnE "(FROM|JOIN|INTO|UPDATE|TABLE) (tenants|tenant_plans|
--      tenant_industries|tenant_n8n_config)"` = 0 hits at HEAD; every match for the bare words is
--      prose — src/handlers/admin_settings_handler.rs:1422 spells the situation out: "the legacy
--      `tenants` table has 0 rows, so the lookup could never match").  The readers that DID exist
--      (`SELECT name FROM tenants ...`, `UPDATE tenants SET ...`) were removed by the tid->aid
--      refactor and are not reachable from HEAD (git log -S, 4 commits, all ancestors).
--   3. 0 ROWS in all four, and no inbound FK edge from any table outside the group, no view,
--      routine, trigger or default in the database mentions them (pg_depend: the only edges are
--      the tables' own constraints/indexes/defaults/row types).  So a plain non-CASCADE drop
--      removes nothing else.
--   4. Keeping them is not neutral: a second, unread `tenants` is exactly the table a future lane
--      writes by mistake (the pre-034 code shape is still the shape in 001/010/031/033, so the
--      mistake is one copy-paste away).
--
-- LIVE SAFETY
--   * Idempotent: `DROP TABLE IF EXISTS`, and it is a NO-OP on a fresh build (034 already renamed
--     these names away before this file runs).
--   * Children before parent, no CASCADE: tenant_industries / tenant_n8n_config / tenant_plans
--     hold the only FK edges into `tenants` (3 internal edges, measured).
--   * No BEGIN/COMMIT: the runner (src/db.rs) wraps each file in one transaction and no other file
--     in this directory declares its own.
--   * Recovery path: full schema+data dumps per table in /opt/swift/audits/t_fd27c832/dumps/
--     (`<table>.sql`, plus a full -Fc dump of the database). The CREATE DDL also survives in
--     001/010/031/033, so each is one migration away from returning.

DROP TABLE IF EXISTS tenant_industries;  -- child of tenants
DROP TABLE IF EXISTS tenant_n8n_config;  -- child of tenants
DROP TABLE IF EXISTS tenant_plans;       -- child of tenants
DROP TABLE IF EXISTS tenants;
