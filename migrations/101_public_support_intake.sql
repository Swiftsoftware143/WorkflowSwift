-- 101_public_support_intake.sql
--
-- A VISITOR who cannot sign in files a support request (2026-11-06). Every support address this
-- product printed lived on a domain publishing NO mail exchanger — measured 2026-11-06 on
-- dns.google and cloudflare-dns.com: MX = 0 for swiftsoftware.net, workflowswift.com and the rest
-- of the fleet, and nothing on this host listens on port 25 — so a locked-out customer's "write to
-- support@…" message BOUNCED and they stayed locked out.
--
-- The contact page now files that message into the operator's existing ticket queue through
-- `POST /api/v1/public/support-request`. Such a ticket belongs to no workspace, so `aid` must be
-- allowed to be NULL, and the only address the team can answer is the one the visitor typed:
-- `requested_by` carries it. The signed-in arm leaves it '' and keeps using `aid`.
--
-- Idempotent by construction: `sqlx::migrate!` applies this file once, and the statements also
-- carry the repeatable form so a rehearsal or re-run can never fail the boot.
ALTER TABLE tickets ALTER COLUMN aid DROP NOT NULL;

ALTER TABLE tickets ADD COLUMN IF NOT EXISTS requested_by text NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_tickets_requested_by
    ON tickets (lower(requested_by), created_at DESC)
    WHERE requested_by <> '';
