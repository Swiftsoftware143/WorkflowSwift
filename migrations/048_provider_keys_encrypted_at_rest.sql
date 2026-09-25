-- 048_provider_keys_encrypted_at_rest.sql
--
-- provider_keys.api_key holds CUSTOMER-SUPPLIED third-party credentials (OpenAI, Resend /
-- SendGrid, Mailgun, social tokens). Before this migration the canonical write path bound the
-- raw request value straight into the column, so every BYOK key a customer entered sat in the
-- clear at rest and would fall out of any database dump or leaked backup.
--
-- The app now encrypts before it writes: AES-256 via pgcrypto, master key held ONLY in the
-- process environment (PROVIDER_KEY_ENC_SECRET), stored as 'enc:v1:' + base64 ciphertext.
-- See src/security/provider_key_crypto.rs for the format and the fail-closed rule.
--
-- This constraint is the regression guard: a future writer that forgets to encrypt FAILS
-- CLOSED at the database instead of silently storing a plaintext credential. An empty string
-- stays allowed so an empty slot is still representable.
--
-- NOT VALID by design: rows written before today (legacy plaintext) are exempt so the app
-- keeps reading them, while every NEW insert/update is checked. After the one-off backfill
-- the constraint is validated once with
--     ALTER TABLE provider_keys VALIDATE CONSTRAINT provider_keys_api_key_encrypted
-- Both statements below are independently valid and idempotent (the app migration runner
-- splits each file on the statement separator and swallows failures).

ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_api_key_encrypted;

ALTER TABLE provider_keys ADD CONSTRAINT provider_keys_api_key_encrypted CHECK (api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;

-- The documented second half, now IN this file (card t_4ebd6f98). Production carries this constraint
-- as VALIDATED (`convalidated = true` — the one-off backfill ran there), while a from-zero build left
-- it NOT VALID and therefore differed from production on the one property the constraint is about.
-- Validating an empty table is instant, and on a database that already validates it this statement is
-- a no-op, so it is safe in both directions. `NOT VALID` remains the state *this file creates* for
-- pre-existing rows; validation happens immediately after, which is what production has.
ALTER TABLE provider_keys VALIDATE CONSTRAINT provider_keys_api_key_encrypted;
