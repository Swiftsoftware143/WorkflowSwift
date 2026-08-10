-- 021: Seed all 13 template categories with icons
-- Adds icon column if not present and upserts the 13 required categories

-- Add icon column to existing template_categories table
ALTER TABLE template_categories ADD COLUMN IF NOT EXISTS icon VARCHAR(50) DEFAULT '📁';

-- Upsert the 13 industry categories with fixed UUIDs for determinism
INSERT INTO template_categories (slug, name, description, icon, sort_order, is_active)
VALUES
    ('sales-lead-gen',         'Sales & Lead Generation',       'Lead capture, nurturing, and sales pipeline automation for real estate, insurance, auto, solar, and more',       '💼', 0,  true),
    ('service-businesses',     'Service Businesses',            'Estimate, schedule, invoice, and rebooking workflows for contractors, cleaners, landscapers, photographers',     '🔧', 1,  true),
    ('recruitment-staffing',   'Recruitment & Staffing',        'Resume screening, interview coordination, placement, and gig economy worker management',                         '👥', 2,  true),
    ('marketing-agencies',     'Marketing Agencies',            'Content calendars, client onboarding, ad campaign management, and reporting workflows',                            '📣', 3,  true),
    ('professional-services',  'Professional Services',         'Tax prep, legal intake, consulting engagement, and billing workflows for accountants, lawyers, consultants',      '⚖️', 4,  true),
    ('ecommerce-retail',       'Ecommerce & Retail',            'Order fulfillment, supplier management, dropshipping, and inventory tracking workflows',                          '🛒', 5,  true),
    ('healthcare-wellness',    'Healthcare & Wellness',         'Patient intake, appointment scheduling, treatment planning, and fitness client management',                       '🏥', 6,  true),
    ('construction-development', 'Construction & Development',  'Permit management, subcontractor bidding, property maintenance, and development tracking',                        '🏗️', 7,  true),
    ('grant-funding',          'Grant & Funding',               'Grant writing, research, submission tracking, and fundraising donor management',                                   '💰', 8,  true),
    ('education-training',     'Education & Training',          'Course creation, student onboarding, enrollment, and certificate management',                                     '📚', 9,  true),
    ('publishing-media',       'Publishing & Media',            'Content approval pipelines, newsletter curation, editorial calendars, and publishing workflows',                  '📰', 10, true),
    ('site-flipping',          'Site Flipping',                 'Website and software project tracking, marketplace listings, sales tracking, and TinyBrander funnel management',  '🔄', 11, true),
    ('government-contracting', 'Government Contracting',        'Government contracting workflows for opportunity discovery, bidding, and contract management',                     '🏛️', 12, true)
ON CONFLICT (slug) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    icon = EXCLUDED.icon,
    sort_order = EXCLUDED.sort_order,
    is_active = true;
