-- 104 — the PROGRAMMATIC key becomes retrievable (kanban t_6c39f2b6).
--
-- WorkflowSwift has TWO key tables. 088 gave `user_api_keys` a sealed copy so its owner can read a
-- key back, but that table authenticates NOTHING. The credential a `Authorization: Bearer` call and
-- the Chrome extension actually present is a row in `api_keys` (`workflowswift_…`) — verified by
-- `auth::api_key_auth::authenticate` — and that table only had create + revoke, so the key a
-- customer could see was not the key that works.
--
-- Same shape as 088, no new design:
--   * key_hash      — argon2, the only thing the authenticator reads;
--   * key_encrypted — the plaintext sealed with the fleet provider-key envelope (`enc:v1:…`,
--                     pgp_sym_encrypt under PROVIDER_KEY_ENC_SECRET), used ONLY for display/reveal.
--
-- The CHECK constraint is what makes a future writer that forgets to seal FAIL CLOSED instead of
-- storing a plaintext credential in the clear (same guard as provider_keys / user_api_keys).
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS key_encrypted text;

ALTER TABLE api_keys DROP CONSTRAINT IF EXISTS api_keys_key_encrypted_envelope;
ALTER TABLE api_keys ADD CONSTRAINT api_keys_key_encrypted_envelope
    CHECK (key_encrypted IS NULL OR key_encrypted LIKE 'enc:v1:%');
