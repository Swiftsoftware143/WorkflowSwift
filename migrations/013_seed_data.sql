-- Seed plan tiers
INSERT INTO plan_tiers (id, name, slug, description, price_monthly, price_yearly, features, sort_order, is_active)
VALUES
    (uuid_generate_v4(), 'Free', 'free', 'Get started with basic workflow automation', 0.00, 0.00, '{"workflows": 3, "instances": 10, "users": 2, "support": "community"}', 0, true),
    (uuid_generate_v4(), 'Starter', 'starter', 'For growing teams needing more power', 29.00, 290.00, '{"workflows": 15, "instances": 100, "users": 10, "support": "email"}', 1, true),
    (uuid_generate_v4(), 'Professional', 'professional', 'For businesses requiring full automation', 79.00, 790.00, '{"workflows": "unlimited", "instances": 1000, "users": 25, "support": "priority"}', 2, true),
    (uuid_generate_v4(), 'Enterprise', 'enterprise', 'Custom solutions for large organizations', 199.00, 1990.00, '{"workflows": "unlimited", "instances": "unlimited", "users": "unlimited", "support": "dedicated", "sla": true}', 3, true)
ON CONFLICT (slug) DO NOTHING;

-- Seed template categories
-- Marketing templates
INSERT INTO workflow_templates (id, name, description, category, is_public)
SELECT uuid_generate_v4(), 'Lead Generation', 'Capture and nurture leads through automated outreach and follow-up sequences', 'marketing', true
WHERE NOT EXISTS (SELECT 1 FROM workflow_templates WHERE name = 'Lead Generation' AND category = 'marketing');

INSERT INTO workflow_templates (id, name, description, category, is_public)
SELECT uuid_generate_v4(), 'Newsletter Campaign', 'Design and execute email newsletter campaigns with subscriber management', 'marketing', true
WHERE NOT EXISTS (SELECT 1 FROM workflow_templates WHERE name = 'Newsletter Campaign' AND category = 'marketing');

-- Operations templates
INSERT INTO workflow_templates (id, name, description, category, is_public)
SELECT uuid_generate_v4(), 'Client Onboarding', 'Streamline new client intake, document collection, and account setup', 'operations', true
WHERE NOT EXISTS (SELECT 1 FROM workflow_templates WHERE name = 'Client Onboarding' AND category = 'operations');

INSERT INTO workflow_templates (id, name, description, category, is_public)
SELECT uuid_generate_v4(), 'Project Delivery', 'End-to-end project management from kickoff to delivery and handoff', 'operations', true
WHERE NOT EXISTS (SELECT 1 FROM workflow_templates WHERE name = 'Project Delivery' AND category = 'operations');

-- Government Contracting template
INSERT INTO workflow_templates (id, name, description, category, is_public)
SELECT uuid_generate_v4(), 'Government Contracting Lifecycle', 'Complete 10-step government contracting workflow from discovery to contract management', 'government-contracting', true
WHERE NOT EXISTS (SELECT 1 FROM workflow_templates WHERE name = 'Government Contracting Lifecycle' AND category = 'government-contracting');

-- Seed steps for Government Contracting template
DO $$
DECLARE
    gov_template_id UUID;
BEGIN
    SELECT id INTO gov_template_id FROM workflow_templates WHERE name = 'Government Contracting Lifecycle' AND category = 'government-contracting' LIMIT 1;

    IF gov_template_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM workflow_template_steps WHERE template_id = gov_template_id) THEN
        INSERT INTO workflow_template_steps (id, template_id, step_type, name, description, sort_order) VALUES
            (uuid_generate_v4(), gov_template_id, 'discovery', 'Discover', 'Identify government contracting opportunities through market research and agency analysis', 0),
            (uuid_generate_v4(), gov_template_id, 'qualify', 'Qualify', 'Evaluate eligibility, bonding capacity, and alignment with agency requirements', 1),
            (uuid_generate_v4(), gov_template_id, 'team', 'Team', 'Assemble project team, identify subcontractors, and establish partnerships', 2),
            (uuid_generate_v4(), gov_template_id, 'propose', 'Propose', 'Prepare and submit comprehensive proposal including technical approach and pricing', 3),
            (uuid_generate_v4(), gov_template_id, 'submit', 'Submit', 'Submit final proposal through designated procurement system (SAM.gov, etc.)', 4),
            (uuid_generate_v4(), gov_template_id, 'track', 'Track', 'Monitor submission status, respond to clarifications, and track evaluation progress', 5),
            (uuid_generate_v4(), gov_template_id, 'manage', 'Manage', 'Manage awarded contract including compliance reporting and milestone delivery', 6),
            (uuid_generate_v4(), gov_template_id, 'intel', 'Intel', 'Gather competitive intelligence on agency purchasing patterns and competitor awards', 7),
            (uuid_generate_v4(), gov_template_id, 'outreach', 'Outreach', 'Conduct proactive outreach to agency decision makers and contracting officers', 8),
            (uuid_generate_v4(), gov_template_id, 'dashboard', 'Dashboard', 'Monitor pipeline metrics, win rates, and contracting performance analytics', 9);
    END IF;
END $$;

-- Seed credit packages
INSERT INTO credit_packages (id, name, credits, price, is_active)
SELECT uuid_generate_v4(), name, credits, price, true
FROM (VALUES
    ('Starter Pack', 100, 9.99),
    ('Growth Pack', 500, 39.99),
    ('Pro Pack', 2000, 149.99),
    ('Enterprise Pack', 10000, 599.99)
) AS p(name, credits, price)
WHERE NOT EXISTS (SELECT 1 FROM credit_packages WHERE name = p.name);
