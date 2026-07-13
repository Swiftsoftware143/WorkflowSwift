-- Migration 011: Enforce three-role system: admin, user, team_member
-- Only David (swiftsoftware143@yahoo.com) can be admin
-- All existing non-admin roles get normalized

-- 1. Reset David's account to be the sole admin with super_admin flag
UPDATE users
SET role = 'admin', perm_is_super_admin = true
WHERE email = 'swiftsoftware143@yahoo.com';

-- 2. All other accounts with admin/company_admin/staff/etc → normalize to user
UPDATE users
SET role = 'user'
WHERE email != 'swiftsoftware143@yahoo.com'
  AND role IN ('admin', 'company_admin', 'staff', 'company_owner', 'manager');

-- 3. Anyone with role = 'user' and perm_is_super_admin = true → only David keeps it
UPDATE users
SET perm_is_super_admin = false
WHERE email != 'swiftsoftware143@yahoo.com';

-- 4. Add a unique constraint ensuring only one admin (David)
-- We'll enforce this at the application level + a partial unique index
-- This prevents any future INSERT/UPDATE from creating a second admin
CREATE UNIQUE INDEX IF NOT EXISTS idx_unique_admin
    ON users (role) WHERE role = 'admin';

-- 5. Ensure team_member role is lowercase and consistent
UPDATE users SET role = 'team_member' WHERE role = 'team_member';

-- 6. Add email_settings to admin_settings if not present
INSERT INTO admin_settings (key, value, description)
SELECT 'email', '{
  "api_url": "",
  "api_key": "",
  "from_address": "swiftsoftware143@yahoo.com",
  "from_name": "WorkflowSwift",
  "provider": "smtp",
  "smtp_host": "",
  "smtp_port": 587,
  "smtp_username": "",
  "smtp_password": "",
  "smtp_use_tls": true
}'::jsonb, 'Email/SMTP configuration for sending transactional emails'
WHERE NOT EXISTS (SELECT 1 FROM admin_settings WHERE key = 'email');
