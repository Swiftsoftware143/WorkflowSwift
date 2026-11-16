-- 115_accounts_industry_slug_no_default.sql
--
-- A NEW WORKSPACE IS NOT A SITE FLIPPER (2026-11-16, cron `default`).
--
-- `accounts.industry_slug` shipped with DEFAULT 'site-flipping' and the public register door
-- passed that same literal when the signup named no industry (`src/auth/handlers.rs`,
-- `req.industry_slug.as_deref().unwrap_or("site-flipping")`). The console titles the first screen
-- from that value ("📊 Site Flipping Dashboard — Live KPIs for Site Flipping") and seeds the
-- industry's dashboard + default widgets from it, so every customer who signed up for
-- WorkflowSwift — a real-estate agency, a SaaS founder, a government contractor — landed on a
-- site-flipping dashboard they never asked for, and the console's own "Pick your industry" prompt
-- never appeared because a slug was always present. Measured live before this file: 12 of 13
-- accounts carried `site-flipping`, including the operator's own workspaces.
--
-- The empty state is a SUPPORTED state, not a hole: the console renders the picker card and the
-- sentence "Live KPIs — choose your industry from the picker and this dashboard fills in from
-- your own automations" whenever the account has no industry (that branch already existed for a
-- workspace whose industry read failed), and a NULL row is what a canary account already holds.
--
-- So the default goes, and the register door stops inventing a value. NULL now means exactly one
-- thing: this customer has not picked an industry yet.
--
-- Idempotent on purpose: this app's runner re-tries any file it could not record in `_migrations`,
-- and a fresh install applies every file in order. `DROP DEFAULT` on a column that has none is a
-- no-op, and a COMMENT is a replace.
ALTER TABLE accounts ALTER COLUMN industry_slug DROP DEFAULT;

COMMENT ON COLUMN accounts.industry_slug IS
  'Industry whose dashboard this workspace was seeded from. NULL = the customer has not picked one yet, and the console shows the industry picker (POST /api/v1/accounts/industry). Deliberately NO default: a fabricated one gave every new signup another industry''s dashboard (migration 115, 2026-11-16).';
