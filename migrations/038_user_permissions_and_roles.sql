-- Migration 038: User permissions restructuring
-- Phase 1: Add perm_is_super_admin and permissions JSONB to users table
-- Phase 4: Extend email_templates with template_type, html_body, is_html, is_default

-- ============================================================
-- Part 1: User table changes
-- ============================================================

-- Add super_admin boolean flag (only David should have true)
ALTER TABLE users ADD COLUMN IF NOT EXISTS perm_is_super_admin BOOLEAN NOT NULL DEFAULT false;

-- Add permissions JSONB for granular team-member permissions
ALTER TABLE users ADD COLUMN IF NOT EXISTS permissions JSONB NOT NULL DEFAULT '{}'::jsonb;

-- Set David Admin as the super_admin (swiftsoftware143@yahoo.com)
UPDATE users SET perm_is_super_admin = true WHERE email = 'swiftsoftware143@yahoo.com';

-- ============================================================
-- Part 2: Email templates table changes
-- ============================================================

-- Add template_type to email_templates
ALTER TABLE email_templates ADD COLUMN IF NOT EXISTS template_type TEXT;
ALTER TABLE email_templates ADD COLUMN IF NOT EXISTS html_body TEXT;
ALTER TABLE email_templates ADD COLUMN IF NOT EXISTS is_html BOOLEAN DEFAULT true;
ALTER TABLE email_templates ADD COLUMN IF NOT EXISTS is_default BOOLEAN DEFAULT false;

-- Seed default email templates

-- Welcome email template
INSERT INTO email_templates (id, aid, name, subject, body, html_body, template_type, is_html, is_default)
SELECT
    'a0000000-0000-0000-0000-000000000001'::uuid,
    '00000000-0000-0000-0000-000000000000'::uuid,
    'Welcome Email',
    'Welcome to WorkflowSwift!',
    'Hello {{name}},

Welcome to WorkflowSwift! Your account has been created.

Here are your login credentials:
  Email: {{email}}
  Temporary Password: {{password}}

Please log in at {{app_url}} and change your password.

Best regards,
The WorkflowSwift Team',
    '<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
  <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
    <div style="text-align: center; padding: 20px 0;">
      <h1 style="color: #2563eb;">Welcome to WorkflowSwift!</h1>
    </div>
    <p>Hello <strong>{{name}}</strong>,</p>
    <p>Welcome to WorkflowSwift! Your account has been created successfully.</p>
    <div style="background: #f3f4f6; padding: 15px; border-radius: 8px; margin: 20px 0;">
      <p style="margin: 5px 0;"><strong>Email:</strong> {{email}}</p>
      <p style="margin: 5px 0;"><strong>Temporary Password:</strong> {{password}}</p>
    </div>
    <p>Please log in and change your password as soon as possible.</p>
    <div style="text-align: center; margin: 30px 0;">
      <a href="{{app_url}}" style="background: #2563eb; color: white; padding: 12px 30px; text-decoration: none; border-radius: 6px; font-weight: bold;">Log In Now</a>
    </div>
    <p style="color: #666; font-size: 12px; text-align: center; margin-top: 40px;">
      Best regards,<br>The WorkflowSwift Team
    </p>
  </div>
</body>
</html>',
    'welcome',
    true,
    true
WHERE NOT EXISTS (SELECT 1 FROM email_templates WHERE template_type = 'welcome');

-- Team invite email template
INSERT INTO email_templates (id, aid, name, subject, body, html_body, template_type, is_html, is_default)
SELECT
    'a0000000-0000-0000-0000-000000000002'::uuid,
    '00000000-0000-0000-0000-000000000000'::uuid,
    'Team Invite',
    'You''ve Been Invited to WorkflowSwift',
    'Hello {{name}},

You have been invited to join {{account_name}} on WorkflowSwift!

Here are your login credentials:
  Email: {{email}}
  Temporary Password: {{password}}

Please log in at {{app_url}} and change your password.

Best regards,
The WorkflowSwift Team',
    '<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
  <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
    <div style="text-align: center; padding: 20px 0;">
      <h1 style="color: #2563eb;">Team Invitation</h1>
    </div>
    <p>Hello <strong>{{name}}</strong>,</p>
    <p>You have been invited to join <strong>{{account_name}}</strong> on WorkflowSwift!</p>
    <div style="background: #f3f4f6; padding: 15px; border-radius: 8px; margin: 20px 0;">
      <p style="margin: 5px 0;"><strong>Email:</strong> {{email}}</p>
      <p style="margin: 5px 0;"><strong>Temporary Password:</strong> {{password}}</p>
    </div>
    <p>Please log in and change your password as soon as possible.</p>
    <div style="text-align: center; margin: 30px 0;">
      <a href="{{app_url}}" style="background: #2563eb; color: white; padding: 12px 30px; text-decoration: none; border-radius: 6px; font-weight: bold;">Log In Now</a>
    </div>
    <p style="color: #666; font-size: 12px; text-align: center; margin-top: 40px;">
      Best regards,<br>The WorkflowSwift Team
    </p>
  </div>
</body>
</html>',
    'team_invite',
    true,
    true
WHERE NOT EXISTS (SELECT 1 FROM email_templates WHERE template_type = 'team_invite');

-- Password reset email template (keep existing but add template for consistency)
INSERT INTO email_templates (id, aid, name, subject, body, html_body, template_type, is_html, is_default)
SELECT
    'a0000000-0000-0000-0000-000000000003'::uuid,
    '00000000-0000-0000-0000-000000000000'::uuid,
    'Password Reset',
    'Password Reset Request',
    'Your password reset code is: {{token}}

This code expires in 1 hour.

If you did not request this password reset, please ignore this email.

- The WorkflowSwift Team',
    '<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
  <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
    <div style="text-align: center; padding: 20px 0;">
      <h1 style="color: #2563eb;">Password Reset</h1>
    </div>
    <p>You have requested a password reset.</p>
    <div style="background: #f3f4f6; padding: 15px; border-radius: 8px; margin: 20px 0; text-align: center;">
      <p style="font-size: 24px; font-weight: bold; letter-spacing: 3px;">{{token}}</p>
    </div>
    <p>This code expires in <strong>1 hour</strong>.</p>
    <p style="color: #666;">If you did not request this password reset, please ignore this email.</p>
    <p style="color: #666; font-size: 12px; text-align: center; margin-top: 40px;">
      - The WorkflowSwift Team
    </p>
  </div>
</body>
</html>',
    'password_reset',
    true,
    true
WHERE NOT EXISTS (SELECT 1 FROM email_templates WHERE template_type = 'password_reset');
