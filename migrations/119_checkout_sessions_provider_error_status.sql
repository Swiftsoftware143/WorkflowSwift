-- 119: `checkout_sessions.status` learns `provider_error` — the state a checkout attempt is in
-- when a CONFIGURED payment provider REFUSES to create the session (kanban t_166de88e; the same
-- fix FunnelSwift shipped as d9f10d8).
--
-- THE DEFECT: `create_checkout_session` called `create_stripe_session(...).await?`, so a rejected
-- key or a provider outage propagated an `AppError::UpstreamRefused` — the buyer read the
-- provider's own operator-facing sentence, nothing was filed in `tickets`, no receipt was mailed
-- and the operator was never told. The refusal is now recorded here, as its own status: `failed`
-- means a payment went wrong AFTER the provider accepted the session, and `pending` promises a
-- webhook that a session the provider never created can never send. Neither is the truth here.
--
-- Additive only: every value that was legal stays legal (the 096 list is repeated verbatim) and no
-- row is rewritten. Idempotent, because this app re-runs every file in ./migrations at boot
-- (src/db.rs); the filename ledger means a NEW file reaches production at the next boot.
ALTER TABLE checkout_sessions DROP CONSTRAINT IF EXISTS checkout_sessions_status_check;
ALTER TABLE checkout_sessions ADD CONSTRAINT checkout_sessions_status_check
  CHECK (status = ANY (ARRAY['pending'::text, 'completed'::text, 'failed'::text,
                             'expired'::text, 'refunded'::text, 'payment_link'::text,
                             'provider_error'::text]));
