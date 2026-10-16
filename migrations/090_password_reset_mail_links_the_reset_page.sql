-- 090_password_reset_mail_links_the_reset_page.sql
-- The password-reset mail sent the customer to a page that cannot accept the code, and promised a
-- validity the server does not mint.
--
-- MEASURED LIVE 2026-10-16 (fleet-owned capture sink, app 4246dd9): the message actually sent is this
-- row, not the hard-coded arm in src/email.rs. It read
--
--   Open {{login_url}} and click "Forgot password?", then enter the code above to choose a new
--   password.
--
-- `{{login_url}}` is the bare app origin, and app.workflowswift.com/login has NO code field — its only
-- inputs are email / password (plus the register tab's name+email) and its own "Forgot password?"
-- link goes to /forgot-password.html, which asks for the ADDRESS and mails ANOTHER code. So a customer
-- who forgot their password followed the mail, landed on a page with nowhere to put the code, and
-- clicking the only link offered just produced another code. The one page that accepts the code —
-- /reset-password (input id="t", prefilled from `?token=`) — was never named.
--
-- The same body also said "This code expires in 1 hour" while auth::handlers::forgot_password mints
-- the token for 24 hours (`Utc::now() + chrono::Duration::hours(24)`), and the seeded text was even
-- shorter than the live row (the live wording was written straight into the database on 2026-10-13,
-- which is why migration 012 still holds a different string).
--
-- This migration writes the corrected text into the SHIPPED default row, so a fresh install and live
-- agree, and updates it in place on every database that already carries it. Placeholders are the ones
-- `render_template` already binds: {{name}}, {{app_name}}, {{token}}, {{login_url}} (== app_url).
-- Nothing else about the send changes; nothing is sent by this migration.
--
-- Idempotent by construction: the same UPDATE on the same single row yields the same values.

UPDATE email_templates
   SET body = E'Hi {{name}},\n\nWe received a request to reset the password on your {{app_name}} account.\n\nYour reset code is:\n\n{{token}}\n\nOpen {{login_url}}/reset-password?token={{token}} to choose a new password - the code is filled in for you. If that link does not open the form, go to {{login_url}}/reset-password and paste the code above.\n\nThe code expires in 24 hours, so use it soon.\n\nIf you didn''t ask for a password reset, you don''t need to do anything - your password stays the same and no changes have been made to your account.\n\n- The WorkflowSwift Team',
       html_body = E'<h2 style="margin:0 0 12px;font-size:18px;">Reset your password</h2>\n<p style="margin:0 0 12px;">Hi {{name}},</p>\n<p style="margin:0 0 12px;">We received a request to reset the password on your {{app_name}} account.</p>\n<p style="margin:0 0 12px;font-size:20px;"><code>{{token}}</code></p>\n<p style="margin:0 0 12px;">Open <a href="{{login_url}}/reset-password?token={{token}}">{{login_url}}/reset-password</a> and choose a new password - the reset code is filled in for you.</p>\n<p style="margin:0 0 12px;">If that link does not open the form, go to <strong>{{login_url}}/reset-password</strong> and paste the code above.</p>\n<p style="margin:0 0 12px;">This code expires in 24 hours, so use it soon.</p>\n<p style="margin:0 0 12px;">If you didn''t ask for a password reset, you don''t need to do anything - your password stays the same and no changes have been made to your account.</p>\n<p style="margin:0;">- The WorkflowSwift Team</p>',
       updated_at = NOW()
 WHERE id = '00000000-0000-0000-0000-000000000003'
   AND template_type = 'password_reset';
