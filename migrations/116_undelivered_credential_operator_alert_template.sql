-- 116_undelivered_credential_operator_alert_template.sql
--
-- THE OPERATOR'S "A CUSTOMER CANNOT SIGN IN" ALERT NEEDS WORDS (kanban t_177ac7ce, 2026-11-16).
--
-- `POST /api/v1/auth/register` mints the workspace, the owner and the credentials mail, and latches
-- `users.password_undelivered` when that send does not go. Until this file, the only trace was a
-- badge on the admin console's Accounts screen — read by somebody who happens to open it. A customer
-- measured on the sibling app (ADASwift, 2026-11-15) sat locked out of the product they had just
-- signed up for for two days with nobody told.
--
-- `src/credential_watch.rs` now mails the operator for exactly that event. This app's one send funnel
-- (`email::send_email`) resolves every message from an `email_templates` row, so the alert needs its
-- own row here or the operator would receive the generic "WorkflowSwift Notification" body instead of
-- the account details. The row is GLOBAL (`aid` NULL) and the default for its type: the alert is
-- platform mail about a tenant, never a tenant's own mail.
--
-- Idempotent by construction: this app reads `migrations/` with `read_dir` and re-runs any file it
-- could not record in `_migrations`, and a fresh install applies every file in order. The INSERT is
-- guarded by NOT EXISTS on the same (template_type, aid IS NULL, is_default) identity its unique
-- index enforces, so re-applying it changes nothing and an operator who has edited the row keeps
-- their edit.
--
-- Placeholders are the funnel's single-brace form (`{workspace}`, `{who}`, ...): `email::render_template`
-- substitutes both spellings, and an unknown key is reported rather than silently left in the body.

INSERT INTO email_templates (template_type, name, subject, body, html_body, is_default, is_html, is_active)
SELECT
    'undelivered_credential_operator_alert',
    'Undelivered first password — operator alert',
    'A customer cannot sign in — no password was delivered',
    $body${workspace} signed up, but the email carrying the first password never reached the
customer, so they cannot sign in.

Workspace: {workspace}
Customer: {who}
Signed up: {signed_up}

A password reset travels the same mail path that just failed, so it may not reach them either.
The admin console's Accounts screen badges that row "password never delivered" and its
"Send login" button mints a new password and emails it — that is the way back in.

Admin console: {admin_url}$body$,
    $html$<p><strong>{workspace}</strong> signed up, but the email carrying the first password never
reached the customer, so they cannot sign in.</p>
<table cellpadding="6" cellspacing="0" style="border-collapse:collapse;font-size:14px;color:#374151">
<tr><td style="color:#6b7280">Workspace</td><td><strong>{workspace}</strong></td></tr>
<tr><td style="color:#6b7280">Customer</td><td>{who}</td></tr>
<tr><td style="color:#6b7280">Signed up</td><td>{signed_up}</td></tr>
</table>
<p>A password reset travels the same mail path that just failed, so it may not reach them either.
The admin console's Accounts screen badges that row <strong>"password never delivered"</strong> and its
<strong>"Send login"</strong> button mints a new password and emails it — that is the way back in.</p>
<p><a href="{admin_url}" style="color:#2563eb">Open the admin console</a></p>$html$,
    true,
    true,
    true
WHERE NOT EXISTS (
    SELECT 1
      FROM email_templates
     WHERE template_type = 'undelivered_credential_operator_alert'
       AND aid IS NULL
       AND is_default = true
);
