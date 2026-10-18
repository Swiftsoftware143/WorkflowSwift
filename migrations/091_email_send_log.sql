-- 091_email_send_log.sql — the HISTORY behind "did that credentials mail go out?"
--
-- `admin_settings.email` keeps ONE last-send record (`last_send_ok` / `last_send_error` /
-- `last_send_at` / `last_send_template`) and every later mail overwrites it. So a welcome or
-- credentials mail the provider refused at signup is erased the moment any other message
-- succeeds, and the Email Provider panel can only ever answer "what happened to the LAST one?".
-- An operator asking "did this customer ever get their password?" had nothing to read.
--
-- This table is append-only, written from the ONE place every send outcome is recorded
-- (`email::record_send_outcome` / `email::record_withheld_send`), so any send path added later
-- inherits it. `withheld` is a first-class status, not a failure: a fleet-harness / reserved
-- recipient is never attempted at all (no provider request), and recording that as a refusal
-- would be the misreport the sibling apps fixed on 2026-10-14.
--
-- Idempotent: this app's boot runner re-reads ./migrations on every start and applies only the
-- files not yet recorded in `_migrations`, but a file must still be safe to re-run.
CREATE TABLE IF NOT EXISTS email_send_log (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    to_email   text        NOT NULL,
    template   text,
    status     text        NOT NULL CHECK (status IN ('sent', 'failed', 'withheld')),
    detail     text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS email_send_log_created_idx ON email_send_log (created_at DESC);
