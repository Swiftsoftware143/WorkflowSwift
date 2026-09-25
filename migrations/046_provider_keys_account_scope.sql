-- 046_provider_keys_account_scope.sql
--
-- Makes the per-account BYOK store actually work in this app.
--
-- provider_keys was created (migrations/027a_provider_keys.sql, formerly provider_keys.sql)
-- against the LEGACY `tenants` table with `tenant_id UUID NOT NULL REFERENCES tenants(id)`.
-- Migration 034 later renamed tenants to accounts, so `tenants` is now empty and the app scopes
-- every account-owned row by `aid`. Nothing ever bound tenant_id on write, so
-- POST /api/v1/provider-keys failed with a NOT NULL violation (HTTP 500 "Database error") and the
-- table stayed empty — the Integration Center's BYOK surface (fleet standard 2026-09-20) could not
-- store a key.
--
-- Fix: the account scope is `aid`. Keep the legacy column (nullable) so old readers do not break,
-- drop the stale foreign key to the deprecated tenants table, and let the handler mirror the
-- account id into tenant_id on write (src/handlers/provider_keys_handler.rs:141 INSERTs BOTH:
-- `INSERT INTO provider_keys (id, aid, tenant_id, provider, api_key, base_url, metadata,
-- is_active)` — so both columns have to exist on a fresh install, not just on this one).
--
-- REVISED for card t_4ebd6f98: this file used to be two unguarded `ALTER TABLE provider_keys`
-- statements that assumed `tenant_id` still exists. On a FROM-ZERO build it does not —
-- 034_rename_tenant_to_account.sql renames it to `aid` (that is the whole point of 034) — so the
-- first statement died with `column "tenant_id" does not exist` and refused the boot. The intent is
-- now expressed in the order a fresh database needs: drop the not-null only where the legacy column
-- survived, drop the stale FK wherever it lives (an FK constraint keeps its own name
-- `provider_keys_tenant_id_fkey` even after 034 renames the COLUMN, which is why this drops it by
-- the old name), and make sure the mirror column the handler writes exists.
--
-- Idempotent and safe to re-run; production has this file recorded in `_migrations` (2026-09-20) so
-- it is skipped there and cannot change anything live: every statement below is either a no-op on
-- that database (tenant_id exists and is already nullable, the FK is already gone, the column is
-- already present) or a guarded no-op.

DO $mig$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'provider_keys' AND column_name = 'tenant_id'
    ) THEN
        ALTER TABLE provider_keys ALTER COLUMN tenant_id DROP NOT NULL;
    END IF;
END $mig$;

ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_tenant_id_fkey;

-- The legacy mirror column the handler writes next to `aid`; absent on a fresh install, where
-- 034 renamed it away, and present on production.
ALTER TABLE provider_keys ADD COLUMN IF NOT EXISTS tenant_id UUID;

-- Match production, where `aid` is NULLABLE (measured: `aid uuid` nullable=YES; 034 renamed a
-- `tenant_id` that 027a had declared NOT NULL, so a fresh build inherits NOT NULL while production
-- does not). No-op on production; on a fresh install it lets a key row be written before an account
-- id is resolved rather than 500ing.
ALTER TABLE provider_keys ALTER COLUMN aid DROP NOT NULL;
