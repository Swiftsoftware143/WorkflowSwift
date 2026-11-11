-- 088 — user API keys: minted with the account, RETRIEVABLE, auditable (kanban t_7863696b).
--
-- David, 2026-11-11: "shown once, never again" is withdrawn. A key is stored twice:
--   * key_hash     — argon2, the only thing the api-key authenticator reads;
--   * key_encrypted — the plaintext sealed with the fleet provider-key envelope (`enc:v1:…`,
--                     pgp_sym_encrypt under PROVIDER_KEY_ENC_SECRET), used ONLY for display.

ALTER TABLE user_api_keys ADD COLUMN IF NOT EXISTS key_encrypted text;

ALTER TABLE user_api_keys DROP CONSTRAINT IF EXISTS user_api_keys_key_encrypted_envelope;
ALTER TABLE user_api_keys ADD CONSTRAINT user_api_keys_key_encrypted_envelope
    CHECK (key_encrypted IS NULL OR key_encrypted LIKE 'enc:v1:%');

CREATE TABLE IF NOT EXISTS api_key_reveals (
    id          uuid PRIMARY KEY,
    key_id      uuid NOT NULL,
    aid         uuid,
    user_id     uuid,
    key_prefix  text,
    ip_address  text,
    user_agent  text,
    revealed_at timestamp with time zone NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_api_key_reveals_key
    ON api_key_reveals (key_id, revealed_at DESC);

CREATE TABLE IF NOT EXISTS fleet_data_backfills (
    name       text PRIMARY KEY,
    applied_at timestamp with time zone NOT NULL DEFAULT NOW(),
    detail     text
);
