-- Migration 100: the operator's WRITTEN answer on a support ticket (2026-11-06).
--
-- WHY: on this app a ticket is one row with a subject and a description, and the operator's only
-- write was `PUT /api/v1/admin/tickets/{id}` with a status. So a customer who asked a question got
-- the row marked "resolved" and was mailed that it had been handled — with no answer in it. The
-- other apps in the fleet carry the answer on the request itself (FunnelSwift `support_requests.reply`
-- 2026-11-06, ADASwift/IncentiveSwift/MissedCall ticket threads), so a customer reads the sentence
-- the operator typed. This is the same shape here: one nullable-in-practice text column plus the
-- timestamp that proves when it was written.
--
-- `reply` is NOT NULL with an empty default so every existing row and every INSERT that does not
-- mention it stays valid: '' means "no answer yet" exactly as NULL would, and the JSON the customer
-- reads is a plain string either way.
--
-- Idempotent by construction: `run_migrations` records this filename in `_migrations`, and the two
-- ADD COLUMN statements also carry IF NOT EXISTS so a re-run (or a hand-run during a rehearsal)
-- can never fail the boot.

ALTER TABLE tickets ADD COLUMN IF NOT EXISTS reply text NOT NULL DEFAULT '';

ALTER TABLE tickets ADD COLUMN IF NOT EXISTS replied_at timestamptz;

-- The operator's queue reads newest-first and the customer reads their own list; neither needs an
-- index on `reply`, so none is added here.
