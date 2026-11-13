-- 113_users_closed_at.sql
--
-- "Close my account" (2026-11-13). The customer's own exit door, ported from FunnelSwift
-- (migration 132), IncentiveSwift and ADASwift, where a customer could already close an account.
--
-- A closure is a STATE, never a DELETE. Every foreign key from an account to its workflows, runs,
-- prospects and logs CASCADEs, so deleting the user row would destroy the business's history as a
-- side effect of a privacy request. `closed_at` records the decision; `users.sessions_valid_from`
-- (already present) is moved one second past NOW() in the same statement, which is what ends every
-- token minted before the closure at the auth boundary, and the handler retires the account's API
-- keys explicitly because those are durable credentials the session line does not cover.
--
-- Idempotent on purpose: this app's runner re-tries any file it could not record in `_migrations`,
-- and a fresh install applies every file in order.
ALTER TABLE users ADD COLUMN IF NOT EXISTS closed_at TIMESTAMPTZ;

COMMENT ON COLUMN users.closed_at IS
  'When the customer closed their own account (POST /api/v1/auth/close-account). NULL = open. Rows are kept, sign-in is refused, sessions and API keys are ended.';
