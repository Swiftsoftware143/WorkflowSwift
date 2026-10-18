-- 092_email_changes.sql — a customer can change their SIGN-IN address (cron 2026-10-18).
--
-- WHY: signup collects NAME + EMAIL only, so ONE typo produced an account that could never
-- receive another message from us and had no way out — the Profile screen had no address field at
-- all and no route anywhere in the app could write `users.email`. The change is PROVEN, not merely
-- requested: the confirmation link goes to the NEW mailbox and only that mailbox can complete it,
-- and `users.email` moves at CONFIRMATION time, never at request time (a mistyped new address must
-- never lock the customer out of their own account).
--
-- Idempotent on purpose: this app's runner (`src/db.rs::run_migrations`) re-reads ./migrations on
-- every boot and only records a file AFTER it applies, so a file that is edited or re-run must not
-- throw. `CREATE ... IF NOT EXISTS` throughout, and no constraint that lacks an IF-NOT-EXISTS form.
--
-- `users` has NO `email_verified` column in this app, so nothing here writes one.
CREATE TABLE IF NOT EXISTS email_changes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    new_email TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One lookup per request/confirm; both go through these.
CREATE INDEX IF NOT EXISTS idx_email_changes_user ON email_changes (user_id);
CREATE INDEX IF NOT EXISTS idx_email_changes_token ON email_changes (token);
