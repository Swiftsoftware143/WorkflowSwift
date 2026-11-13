-- 112 — api_key_events: a lifecycle audit for every api_keys credential event.
--
-- ONE event row per lifecycle transition on the PROGRAMMATIC key table (`api_keys`), written by
-- the same code that performs the transition, on the SAME connection/transaction, so the audit
-- cannot be separated from the operation it records:
--   * 'created'     — mint_api_key_on, the ONE mint writer (signup / create / regenerate / backfill)
--   * 'revoked'     — delete_api_key (soft revoke)
--   * 'regenerated' — regenerate_api_key (the retired key)
--
-- The row carries the key PREFIX only — never key_hash, never key_encrypted, never the plaintext —
-- plus the acting user id when known. An audit write failure aborts the operation (fail closed):
-- a credential event that cannot be recorded is not performed.
CREATE TABLE IF NOT EXISTS api_key_events (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    key_id        uuid,
    owner_id      uuid,
    action        text NOT NULL,
    key_prefix    text,
    actor_user_id uuid,
    metadata      jsonb DEFAULT '{}'::jsonb,
    created_at    timestamptz DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_api_key_events_owner_created
    ON api_key_events (owner_id, created_at DESC);
