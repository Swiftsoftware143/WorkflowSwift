-- 096: `checkout_sessions.status` learns `payment_link` — the hand-off a tier's own hosted payment
-- page produces (kanban t_1118e176, port of ADASwift 95c83c5 / IncentiveSwift 1b2cc34d).
--
-- THE DEFECT: `plan_tiers.checkout_url` is a hosted payment page an OPERATOR pastes into a tier (a
-- Stripe Payment Link, a PayPal button, any https page). On this install `payment_providers` holds
-- ZERO rows, so before this change every press on Upgrade could only be answered `503
-- payment_provider_not_configured`, even when the operator already had a working way to be paid.
-- `create_checkout_session` now hands the buyer to that page and records the hand-off HERE.
--
-- WHY A NEW STATUS AND NOT `pending`: `pending` means "a provider session exists and a webhook will
-- confirm it". A hosted page this installation knows nothing about will never send one, so a
-- `pending` row would promise an event that can never arrive and would sit in the operator's history
-- looking like a stuck checkout. The hand-off is its own, honest state: the buyer was sent to pay,
-- and the workspace is seated when the operator resolves the plan request, never by a redirect.
--
-- Additive only: every value that was legal stays legal (the 041 list is repeated verbatim), and no
-- row is rewritten. Applied live as DROP + ADD on a text CHECK — no column change. Idempotent,
-- because this app re-runs every file in ./migrations at boot (src/db.rs).
ALTER TABLE checkout_sessions DROP CONSTRAINT IF EXISTS checkout_sessions_status_check;
ALTER TABLE checkout_sessions ADD CONSTRAINT checkout_sessions_status_check
  CHECK (status = ANY (ARRAY['pending'::text, 'completed'::text, 'failed'::text,
                             'expired'::text, 'refunded'::text, 'payment_link'::text]));
