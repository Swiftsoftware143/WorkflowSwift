-- 077_drop_account_branding_columns.sql
-- WorkflowSwift: DROP the five `accounts` columns that nothing writes and nothing renders —
-- `logo_url`, `branding_name`, `primary_color`, `accent_color` and `custom_domain`.
-- Decision card t_731bf864 ("the accounts branding columns are written and rendered by nothing").
--
-- WHY THESE ARE RESIDUE AND NOT PLANNED SCHEMA (evidence /opt/swift/audits/t_731bf864/):
--   1. 0 WRITERS IN THE CRATE.  `update_account` (PUT /api/v1/accounts) binds ONLY name, slug,
--      hexomatic_key, footer_year, footer_company. Registration (src/auth/handlers.rs:82) inserts
--      (id, name, account_slug, is_active) and the admin-create path
--      (src/handlers/admin_settings_handler.rs:1153) inserts (id, name, slug, account_slug). The
--      `branding fields` the docs used to advertise are bound by NO statement anywhere in src/.
--      `custom_domain` has no writer either, and no Host-header / per-domain routing exists
--      (`grep -rn 'host()|X-Forwarded-Host|server_name' src/` -> 0 hits; sites/workflowswift.conf
--      serves exactly workflowswift.com / app. / admin. — no tenant vhost, no wildcard, no cert).
--   2. 0 RENDERERS.  No console (www-app, www-admin, www) names any of the five; the tenant console
--      calls only /accounts/industry and /accounts/hexomatic-key. The app has NO per-tenant public
--      page, so there is nowhere a tenant logo/colour/domain could appear.
--   3. 0 NON-NULL VALUES, LIVE.  `SELECT count(logo_url), count(branding_name), count(primary_color),
--      count(accent_color), count(custom_domain) FROM accounts` -> 0/0/0/0/0 over 8 accounts at the
--      decision. Dropping loses no data.
--   4. NO INBOUND EDGE.  No view, routine or trigger references any of the five
--      (information_schema.columns -> only `accounts` itself; pg_constraint on accounts ->
--      tenants_pkey, tenants_slug_key; pg_indexes -> none of the five). The only reference outside
--      the table was the `Account` struct field list (src/models/account.rs), which this change
--      removes in the same commit.
--   5. THE PLAN FLAG THAT ADVERTISED THEM IS ALREADY GONE.  kanban t_413b4aab retired the
--      `custom_branding` plan key (migration 075) precisely because there is no branding mechanism;
--      these columns are the orphan that retirement left behind.
--
-- VERDICT (arm (b) of the card, chosen by measurement not taste):
--   The card's arm (a) — build white-label branding — is refused HERE because it needs a render
--   surface that does not exist (no per-tenant public page anywhere in the crate) AND a pricing
--   call (which tier gets white-labelling) that belongs to David; inventing both inside a
--   schema-tidy card would ship an unpriceable feature with nothing to render it. Arm (b) makes the
--   schema stop advertising a capability the product does not have, which is the same rule
--   t_413b4aab applied to the flag. Reversal below.
--
-- LIVE SAFETY
--   * Idempotent: `DROP COLUMN IF EXISTS` per column, a NO-OP on a fresh build (001 never created
--     them on a tree built from these migrations after 077 lands — see the ordering note).
--   * No BEGIN/COMMIT: the runner (src/db.rs) wraps each file in one transaction.
--   * Recovery — one batch, restores all five empty columns (data was NULL everywhere, so the
--     restored columns are byte-identical in content to the pre-drop state):
--       ALTER TABLE accounts ADD COLUMN IF NOT EXISTS logo_url TEXT;
--       ALTER TABLE accounts ADD COLUMN IF NOT EXISTS primary_color VARCHAR(7);
--       ALTER TABLE accounts ADD COLUMN IF NOT EXISTS accent_color VARCHAR(7);
--       ALTER TABLE accounts ADD COLUMN IF NOT EXISTS custom_domain VARCHAR(255);
--       ALTER TABLE accounts ADD COLUMN IF NOT EXISTS branding_name VARCHAR(255);
--     The types above are the ones recorded from information_schema at the decision
--     (/opt/swift/audits/t_731bf864/10-db-PRE.txt).
--
-- ORDERING NOTE: deploy-app.sh runs the from-zero harness before the recreate, so a from-zero run
-- against a live DB that still has the columns reports drift until the boot applies this file (the
-- same ordering artifact recorded for 067/076). Re-running the harness after the boot is PASS.

ALTER TABLE accounts DROP COLUMN IF EXISTS logo_url;
ALTER TABLE accounts DROP COLUMN IF EXISTS primary_color;
ALTER TABLE accounts DROP COLUMN IF EXISTS accent_color;
ALTER TABLE accounts DROP COLUMN IF EXISTS custom_domain;
ALTER TABLE accounts DROP COLUMN IF EXISTS branding_name;
