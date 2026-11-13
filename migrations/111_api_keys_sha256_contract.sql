-- 111 — CoreSwift hash contract for api_keys (kanban: bring WorkflowSwift keys onto the
-- CoreSwift reference contract).
--
-- ONE ACTIVE KEY PER ACCOUNT, ENFORCED BY THE DATABASE: a partial unique index on
-- (aid) WHERE is_active means a second active row for the same account is refused by
-- the database itself (a mint that forgets the friendly 409 check cannot insert a
-- duplicate). Rows with is_active = false are unaffected, so a revoked key keeps its
-- record and an account can retire one key and hold another.
--
-- The row count was measured before this shipped: no account holds more than one
-- active key, so the index creates cleanly and rejects nothing.
CREATE UNIQUE INDEX IF NOT EXISTS api_keys_one_active_per_aid
    ON api_keys (aid)
    WHERE is_active;
