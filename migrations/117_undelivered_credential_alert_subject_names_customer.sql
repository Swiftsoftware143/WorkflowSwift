-- 117_undelivered_credential_alert_subject_names_customer.sql
--
-- THE LOCKED-OUT-CUSTOMER ALERT MUST SAY WHO IS LOCKED OUT (kanban t_38d2eb66, 2026-11-16).
--
-- Migration 116 seeded the operator's alert with the subject
--   'A customer cannot sign in — no password was delivered'
-- which is the same sentence for every customer. In an inbox or a mail history that reads as a wall
-- of identical lines, so the operator cannot tell WHO is locked out without opening each message —
-- the "names neither the customer nor the account" defect. FunnelSwift fixed the same wording in
-- commit 2332b6c and WorkflowSwift's inline fallback arm (src/email.rs, `send_email_fallback`) now
-- carries the same fix.
--
-- This file fixes the SHIPPED path: the global default `email_templates` row that every send
-- resolves first. The subject now leads with `{who}` — the owner's name with their address, or the
-- address alone when no name was given — which `email::render_template` substitutes from the same
-- variables the body already uses, so the alert reads
--   'A customer cannot sign in — Dana Reed (dana@acme.test) has no password'.
--
-- THE ONCE-PER-ACCOUNT DE-DUPLICATE IS UNTOUCHED. This app's `email_send_log` has no subject column,
-- so `credential_watch`'s "already reported" test keys on `template` + `to_email` + `status = 'sent'`
-- + the account's id inside `detail` (written by `send_undelivered_credential_operator_alert` after a
-- send it read back as `sent`). Nothing here reads or rewrites the subject, so each account is still
-- reported exactly once.
--
-- Idempotent by construction and NON-DESTRUCTIVE: the UPDATE only fires while the row still carries
-- the exact old sentence, so re-applying changes nothing and an operator who wrote their own subject
-- keeps it. (116 stays on disk untouched — it is already applied on the live install, and editing an
-- applied migration is forbidden here: the runner records filenames, so an edited file never re-runs.)
UPDATE email_templates
   SET subject = 'A customer cannot sign in — {who} has no password'
 WHERE template_type = 'undelivered_credential_operator_alert'
   AND aid IS NULL
   AND is_default = true
   AND btrim(subject) = 'A customer cannot sign in — no password was delivered'
