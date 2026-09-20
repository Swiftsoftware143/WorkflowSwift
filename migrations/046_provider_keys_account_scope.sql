-- 046_provider_keys_account_scope.sql
--
-- Makes the per-account BYOK store actually work in this app.
--
-- provider_keys was created (migrations/provider_keys.sql) against the LEGACY `tenants`
-- table with `tenant_id UUID NOT NULL REFERENCES tenants(id)`. Migration 034 later renamed
-- tenants to accounts, so `tenants` is now empty and the app scopes every account-owned row
-- by `aid`. Nothing ever bound tenant_id on write, so POST /api/v1/provider-keys failed with
-- a NOT NULL violation (HTTP 500 "Database error") and the table stayed empty — the
-- Integration Center's BYOK surface (fleet standard 2026-09-20) could not store a key.
--
-- Fix: the account scope is `aid`. Keep the legacy column (nullable) so old readers do not
-- break, drop the stale foreign key to the deprecated tenants table, and let the handler
-- mirror the account id into tenant_id on write.
--
-- Idempotent and safe to re-run. The WorkflowSwift migration runner splits each file on the
-- statement separator and swallows failures, so every statement here is independently valid
-- and comments avoid the separator character.

ALTER TABLE provider_keys ALTER COLUMN tenant_id DROP NOT NULL;

ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_tenant_id_fkey;
