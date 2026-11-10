-- The two DEFAULT transactional templates named a mailbox that cannot receive.
--
-- Measured 2026-11-10: `swiftsoftware.net` publishes NO MX record (dns.google and
-- cloudflare-dns.com both answer an empty answer section) and nothing on this host listens on
-- port 25, so a customer who replied to the welcome mail — or a teammate who replied to an
-- invite — bounced. The app's code-built mail already names the served contact form
-- (`SUPPORT_CONTACT` in src/email.rs, https://app.workflowswift.com/contact.html); these two
-- STORED rows were the remaining place that still handed out `support@swiftsoftware.net`.
--
-- Idempotent by construction: `replace()` is a no-op once the old string is gone, so the boot
-- runner may re-apply this file on every start (this app reads `migrations/` with read_dir).
-- One writer per column: only the two anchor sentences change; nothing else in the body is
-- touched, and a template an operator has since rewritten (no dead address left in it) is not
-- selected at all.

UPDATE email_templates
   SET html_body = replace(
           replace(
               html_body,
               'mailto:support@swiftsoftware.net',
               'https://app.workflowswift.com/contact.html'),
           '>support@swiftsoftware.net</a>',
           '>our contact form</a>')
 WHERE html_body LIKE '%mailto:support@swiftsoftware.net%'
    OR html_body LIKE '%>support@swiftsoftware.net</a>%';

-- The team invite read "Reach out to your team admin at <link>" — an admin cannot be reached
-- through a support channel, and the link was the dead mailbox. Say what is true instead.
UPDATE email_templates
   SET html_body = replace(
           html_body,
           'Questions? Reach out to your team admin at <a href="https://app.workflowswift.com/contact.html"',
           'Questions? Send us a message at <a href="https://app.workflowswift.com/contact.html"')
 WHERE html_body LIKE '%Reach out to your team admin at <a href="https://app.workflowswift.com/contact.html"%';
