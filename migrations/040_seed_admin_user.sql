-- Seed super admin user for WorkflowSwift (idempotent)
-- Password: SwiftAdmin2026!
-- Hash: $2b$12$w9t7GUaZGrSIHZygcnoikOti2997EGKlQP3FSRBd92CluISxD.sOm

-- First ensure the admin account exists
INSERT INTO accounts (id, name, account_slug, is_active)
SELECT 'a0000000-0000-0000-0000-000000000001', 'SwiftSoftware', 'swiftsoftware', true
WHERE NOT EXISTS (SELECT 1 FROM accounts WHERE account_slug = 'swiftsoftware')
ON CONFLICT (account_slug) DO NOTHING;

-- Insert super admin user if not exists
INSERT INTO users (id, aid, email, password_hash, name, role, is_active, perm_is_super_admin, permissions)
SELECT
    'a0000000-0000-0000-0000-000000000002'::uuid,
    (SELECT id FROM accounts WHERE account_slug = 'swiftsoftware' LIMIT 1),
    'admin@swiftsoftware.com',
    '$2b$12$w9t7GUaZGrSIHZygcnoikOti2997EGKlQP3FSRBd92CluISxD.sOm',
    'Super Admin',
    'admin',
    true,
    true,
    '["super_admin"]'::jsonb
WHERE NOT EXISTS (SELECT 1 FROM users WHERE email = 'admin@swiftsoftware.com');
