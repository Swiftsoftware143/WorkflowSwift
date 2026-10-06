-- 080_payment_providers_secrets_encrypted_at_rest.sql
--
-- payment_providers holds TWO CUSTOMER-SUPPLIED credentials, and migration 041 created them under
-- names that promise encryption at rest ("Encrypted API credentials (encrypted-at-rest via
-- app-layer encryption)"), but nothing ever encrypted them: `upsert_payment_provider` bound the
-- raw request value straight into both columns, so a Stripe secret key (sk_live_...) and the
-- endpoint's webhook signing secret (whsec_...) sat in the clear at rest and would fall out of any
-- database dump, leaked backup or read-only SQL grant (kanban t_6104de65).
--
-- WHY THE SIGNING SECRET MATTERS MOST: it is the one credential that decides whether an anonymous
-- POST /api/v1/webhooks/stripe may complete a checkout session and trigger credential delivery.
-- Whoever can read the row can forge a paid event with it.
--
-- The app now seals both before it writes and opens both after it reads: AES-256 via pgcrypto,
-- master key held ONLY in the process environment (PROVIDER_KEY_ENC_SECRET), stored as
-- 'enc:v1:' + base64 ciphertext. See src/security/payment_provider_secrets.rs for the vocabulary
-- and src/security/provider_key_crypto.rs for the format and the fail-closed rule.
--
-- These constraints are the regression guard: a future writer that forgets to seal FAILS CLOSED at
-- the database instead of silently storing a plaintext credential beside sealed ones. An empty
-- string and a NULL stay allowed, because "no credential configured" is representable both ways
-- (041 declares the columns nullable and the upsert binds '' when a field is not submitted).
-- The publishable_key column is deliberately NOT covered: 041 stores it in plaintext for frontend
-- use and its name says so.
--
-- NOT VALID by design: a row written before today (legacy plaintext, e.g. from a restored dump) is
-- exempt so the app keeps reading it through `decrypt_from_storage`'s passthrough, while every NEW
-- insert/update is checked. The VALIDATE half is NOT in this file on purpose: this runner
-- (`src/db.rs`) sends the whole file as one batch and exits the process when a file fails, so a
-- VALIDATE against a restored-plaintext row would refuse to boot. The boot half
-- (`payment_provider_secrets::seal_legacy_payment_provider_secrets`, called from main.rs) seals the
-- legacy rows first and validates only once nothing is left, on every start — which is also the
-- only repair path for a constraint dropped by hand or lost in a restore, because this file is
-- recorded in `_migrations` and never re-runs.

ALTER TABLE payment_providers DROP CONSTRAINT IF EXISTS payment_providers_api_key_encrypted;

ALTER TABLE payment_providers ADD CONSTRAINT payment_providers_api_key_encrypted CHECK (api_key_encrypted = '' OR api_key_encrypted LIKE 'enc:v1:%') NOT VALID;

ALTER TABLE payment_providers DROP CONSTRAINT IF EXISTS payment_providers_webhook_secret_encrypted;

ALTER TABLE payment_providers ADD CONSTRAINT payment_providers_webhook_secret_encrypted CHECK (webhook_secret_encrypted = '' OR webhook_secret_encrypted LIKE 'enc:v1:%') NOT VALID;
