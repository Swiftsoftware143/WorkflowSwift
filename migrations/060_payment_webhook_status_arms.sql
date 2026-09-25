-- 060: `payment_webhook_events.status` learns the two refusal arms the Stripe receiver
-- distinguishes (kanban t_40b77d6a).
--
--   not_configured   the receiver has no signing secret to verify with (no active `stripe` row
--                    in payment_providers, or a row without webhook_secret_encrypted) — a state
--                    an operator fixes from Admin > Payment gateways, so the delivery is answered
--                    503 and Stripe retries it
--   signature_failed a `Stripe-Signature` was presented (or absent) and did not verify
--
-- Before this, both were `failed`: an unverifiable delivery that Stripe will never retry looked
-- exactly like one the receiver could not even accept, so the audit row could not say which
-- happened. Applied live (DROP + ADD on a text CHECK; no row is rewritten, no column changes):
--
--   ALTER TABLE payment_webhook_events DROP CONSTRAINT payment_webhook_events_status_check;
--   ALTER TABLE payment_webhook_events ADD CONSTRAINT payment_webhook_events_status_check
--     CHECK (status = ANY (ARRAY['received','processed','failed','ignored',
--                                'not_configured','signature_failed']));
ALTER TABLE payment_webhook_events DROP CONSTRAINT IF EXISTS payment_webhook_events_status_check;
ALTER TABLE payment_webhook_events ADD CONSTRAINT payment_webhook_events_status_check
  CHECK (status = ANY (ARRAY['received'::text, 'processed'::text, 'failed'::text,
                             'ignored'::text, 'not_configured'::text, 'signature_failed'::text]));
