-- 082_account_email_branding.sql
-- WorkflowSwift: per-ACCOUNT email branding (kanban t_c3cfe7ba, the last of the five fleet apps;
-- FunnelSwift t_c06a32eb, ADASwift c33fcb8, IncentiveSwift, missedcallrespondr already shipped).
--
-- David 2026-10-08: transactional mail should carry the ACCOUNT's own logo / brand name.
--
-- WHERE THE VALUES LIVE (this app has NO `tenants` / `tenant_settings` table — it uses `accounts`
-- and `users.aid`, so branding is keyed by the SAME `aid` every sender already carries):
--
--   * `accounts.settings` — ONE jsonb document per account; the `email_branding` KEY inside it holds
--     `{ brand_name, brand_color, logo_url }`. A generic `settings` column is deliberately chosen
--     over a new key/value table: this app has no per-account settings store today, and a single
--     jsonb column is the smallest surface that the console can read and write.
--
--   * `account_logos` — the logo BYTES. The container is IMAGE-BAKED (bin/deploy-workflowswift.sh ->
--     deploy-app.sh workflowswift; there is no bind-mounted webroot), so a file written at run time
--     dies on the next restart. Bytes in the DB are streamed back by the public logo route, the
--     same decision migration 081 made for `user_avatars`.
--
-- ADDITIVE: `settings` defaults to '{}' and `account_logos` starts empty, so every existing account
-- renders byte-identical mail until it sets something (the renderer adds a header ONLY when a brand
-- name or a logo is actually set).

ALTER TABLE accounts ADD COLUMN IF NOT EXISTS settings jsonb NOT NULL DEFAULT '{}'::jsonb;

CREATE TABLE IF NOT EXISTS account_logos (
    account_id   uuid PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    content_type text        NOT NULL,
    bytes        bytea       NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now()
);
