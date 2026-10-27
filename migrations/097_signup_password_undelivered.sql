-- 097_signup_password_undelivered.sql
--
-- ONE-SHOT LATCH FOR THE SIGNUP DOOR'S OWN PASSWORD.
--
-- This app's signup mints the first password on the server and mails it. When that send fails the
-- account exists with a password nobody has ever seen, so the signup page offers that ONE session a
-- real password field (POST /api/v1/auth/initial-password). The JWT claim `pwd_minted` alone cannot
-- make that offer one-shot: `users.sessions_valid_from` is compared in whole seconds, so a replay
-- inside the same second still passes the boundary check. Measured on the sibling app
-- (missedcallrespondr, 2026-10-27): the second call answered 200 and REPLACED the password the first
-- call had set, and the customer's brand-new credential silently stopped working.
--
-- This column is the exact latch: TRUE means "a first password was minted and never delivered, so
-- this account may still be claimed from the signup session". The flag is cleared in the SAME
-- statement that writes the password (`WHERE ... AND password_undelivered`), so a second attempt
-- updates zero rows and answers 401. Every other password writer (change, reset) clears it too.
--
-- `ADD COLUMN IF NOT EXISTS` + this app's re-execute-when-unrecorded runner means a second run is a
-- no-op, and every existing account (which has a password its owner received) reads `false`.
ALTER TABLE users ADD COLUMN IF NOT EXISTS password_undelivered boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN users.password_undelivered IS
    'TRUE only when signup minted the first password and the credentials mail did not leave; cleared by the one-shot initial-password door, change and reset.';
