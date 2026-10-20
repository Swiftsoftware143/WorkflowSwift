-- A password change or reset must end the sessions that predate it.
--
-- Measured 2026-10-20 (kanban t_80787a0f): a JWT minted BEFORE a password change/reset still
-- answered 200 from a guarded read afterwards, because the boundary trusted the token's claims
-- alone and the token outlives the change.
--
-- NULL means "never restricted": every existing account and every token already in the wild keeps
-- working exactly as before, so this is additive. The column only takes a value when a password is
-- changed or reset, and `auth::middleware` refuses any JWT whose `iat` is older than it.
ALTER TABLE users ADD COLUMN IF NOT EXISTS sessions_valid_from timestamptz;

COMMENT ON COLUMN users.sessions_valid_from IS
    'JWTs issued before this instant are refused (revocation on password change/reset); NULL = never restricted.';
