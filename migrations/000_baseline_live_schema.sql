-- 000: bootstrap baseline — the relations THIS DATABASE has and no other migration creates.
-- Card t_4ebd6f98. Runs FIRST (filename byte order: "000_baseline..." < "001_create_tenants.sql").
--
-- WHY THIS FILE EXISTS
--   `src/db.rs` applies every `./migrations/*.sql` not yet recorded in `_migrations`, one batch per
--   file, and by default an UNRECORDED failure is FATAL (the process exits non-zero). Measured
--   2026-09-25 on an empty database with the runner's own semantics: 30 of 69 files failed and the
--   service refused to boot — 12 relations existed in production with NO migration creating any of
--   them (this file) and 5 files ran before the dependency they need (fixed by rename, see below).
--
--   The twelve: email_templates, feature_limits, industries, provider_categories, renditions,
--   step_types, template_categories, user_api_keys, and the four tables that migration 034 renames
--   into existence (accounts <- tenants, account_plans <- tenant_plans, account_industries <-
--   tenant_industries, n8n_account_config <- tenant_n8n_config — those four are NOT created here,
--   034 creates them, and pre-creating a rename target makes `ALTER TABLE ... RENAME TO` fail).
--
-- WHAT THIS FILE IS NOT
--   Not a dump and not a restore: it carries NO product rows, no tenancy data, no pricing. It is
--   the DDL of eight relations whose shape was read from the live catalog
--   (`pg_get_expr(pg_attrdef)`, `pg_get_constraintdef`, `pg_get_indexdef`) so a from-zero build
--   reaches the same schema production has. Column order, types, nullability and defaults are
--   live's, including the two shapes that look like drift because they ARE drift production
--   carries: `industries.sort_order` is TEXT (not integer) and every table ends with the
--   `sort_order`/`plan_id`/`category_id`/`label`/`is_active` columns added out of band by the
--   admin UI. Reproducing them is the point; "fixing" them here would build a schema the app's
--   readers do not have.
--
-- LIVE SAFETY
--   Every statement is idempotent: `CREATE TABLE IF NOT EXISTS` (no-op where the table exists),
--   inline PRIMARY KEY / UNIQUE (so no `ALTER TABLE ... ADD CONSTRAINT` can collide on a re-run),
--   `CREATE UNIQUE INDEX IF NOT EXISTS`, and `CREATE EXTENSION IF NOT EXISTS`. On the production
--   database all eight tables already exist, so this file writes nothing at all; it is a no-op
--   that gets recorded in `_migrations`, exactly like every other file the runner applies late.
--
-- SEED DATA IS NOT IN THIS FILE. The industry vocabulary (the 6 rows `GET /api/v1/industries`
-- serves) lives in `000a_industry_vocabulary.sql`; the tables here are created empty, as the
-- schema is a different question from the rows.

-- pgcrypto:           gen_random_uuid() on this platform's PostgreSQL 16 is built in, but the
--                     encrypted-at-rest columns use pgp_sym_* (048/053), which is not.
-- "uuid-ossp":        uuid_generate_v4() — the DEFAULT of account_plans.id, created by 034.
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Referenced by 012_seed_email_templates.sql and 038_user_permissions_and_roles.sql.
CREATE TABLE IF NOT EXISTS email_templates (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    template_type text NOT NULL,
    name text NOT NULL,
    subject text NOT NULL,
    body text,
    html_body text,
    is_default boolean DEFAULT false,
    aid uuid,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    is_html boolean DEFAULT true,
    sort_order integer DEFAULT 0,
    plan_id uuid,
    is_active boolean DEFAULT true
);
-- Partial unique index: one default per (template_type, aid) — aid NULL is the global default,
-- which is why the expression index over COALESCE(aid, '0000...') is required.
CREATE UNIQUE INDEX IF NOT EXISTS idx_email_templates_unique
    ON email_templates (template_type, COALESCE(aid, '00000000-0000-0000-0000-000000000000'::uuid), is_default)
    WHERE (aid IS NULL AND is_default = true);

-- Referenced by 031_multi_industry_dashboards.sql step 6 (per-plan max_industries).
CREATE TABLE IF NOT EXISTS feature_limits (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    aid uuid,
    feature_key text NOT NULL,
    max_value integer,
    current_value integer DEFAULT 0,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    limit_value integer,
    plan_id uuid,
    sort_order integer DEFAULT 0
);

-- The industry record behind the slugs the platform ships. Read at runtime by
-- src/handlers/admin_settings_handler.rs (`SELECT id FROM industries WHERE slug = $1`).
CREATE TABLE IF NOT EXISTS industries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text,
    slug text UNIQUE,
    icon text,
    description text,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    category_id uuid,
    sort_order text,
    plan_id uuid
);

-- Provider taxonomy for the integration center (category/label/icon per provider).
CREATE TABLE IF NOT EXISTS provider_categories (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text,
    category text,
    slug text UNIQUE,
    description text,
    icon text,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    category_id uuid,
    label text,
    sort_order integer DEFAULT 0,
    plan_id uuid
);

-- Output renditions produced by a workflow instance (036 writes account_renditions; this is the
-- library of rendition definitions the UI lists).
CREATE TABLE IF NOT EXISTS renditions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    aid uuid,
    name text,
    icon text,
    summary text,
    workflow_id uuid,
    stitch_group_id uuid,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    sort_order integer DEFAULT 0,
    plan_id uuid
);

-- The step-type vocabulary the workflow builder offers.
CREATE TABLE IF NOT EXISTS step_types (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text,
    category text,
    slug text UNIQUE,
    description text,
    icon text,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    sort_order integer DEFAULT 0,
    plan_id uuid
);

-- THE industry table: `GET /api/v1/industries` serves these rows as the industry picker
-- (src/handlers/industry_handler.rs:18), `plan_capabilities.industry_slug` and
-- `workflow_templates.category` are compared against `slug`, and its 6 slugs are exactly
-- `industries.slug`. 019 and 021 used to INSERT a 13-row *sector* vocabulary here before the
-- table existed anywhere; those INSERTs are neutralised (card t_4ebd6f98) because a sector row
-- would be served as a bogus industry choice — the same class the 061 card decided.
CREATE TABLE IF NOT EXISTS template_categories (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    slug text NOT NULL UNIQUE,
    description text,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    icon character varying(50) DEFAULT '📁'::character varying,
    sort_order integer DEFAULT 0,
    plan_id uuid
);

-- Per-user API keys (014_api_keys.sql is the per-ACCOUNT table). NEW-NAME-SAFE PRE-034 SHAPE:
-- `tenant_id` is what 034_rename_tenant_to_account.sql renames to `aid`
-- (`ALTER TABLE user_api_keys RENAME COLUMN tenant_id TO aid`), so the column has to exist under the
-- old name for that file to run at all on a fresh database. Production already carries `aid`; this
-- file is a no-op there (`CREATE TABLE IF NOT EXISTS`).
CREATE TABLE IF NOT EXISTS user_api_keys (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL,
    tenant_id uuid,
    name text,
    key_prefix text,
    key_hash text,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    key_type text,
    permissions jsonb DEFAULT '{}'::jsonb,
    plan_id uuid,
    label text,
    last_used_at timestamp with time zone,
    sort_order integer DEFAULT 0
);
