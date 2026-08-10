-- 022: Seed default dashboard widgets per industry
-- Creates industry_widgets table holding default widget configurations for each industry

CREATE TABLE IF NOT EXISTS industry_widgets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    industry_slug VARCHAR(100) NOT NULL,
    widget_title VARCHAR(255) NOT NULL,
    widget_type VARCHAR(100) NOT NULL DEFAULT 'stat-counter',
    metric_key VARCHAR(255),
    subtitle TEXT,
    icon VARCHAR(10),
    sort_order INT NOT NULL DEFAULT 0,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (industry_slug, widget_title)
);

-- Seed default widgets for all 13 industries
INSERT INTO industry_widgets (industry_slug, widget_title, widget_type, metric_key, subtitle, icon, sort_order)
VALUES
    -- Sales & Lead Generation
    ('sales-lead-gen',         'Leads Generated',     'stat-counter', 'leads_generated',     'New leads this period',     '📈', 0),
    ('sales-lead-gen',         'Conversion Rate',     'stat-counter', 'conversion_rate',     'Lead-to-customer %',        '🎯', 1),
    ('sales-lead-gen',         'Pipeline Value',      'stat-counter', 'pipeline_value',      'Total deal value in pipeline', '💰', 2),
    ('sales-lead-gen',         'Meetings Booked',     'stat-counter', 'meetings_booked',     'Scheduled appointments',    '📅', 3),

    -- Service Businesses
    ('service-businesses',     'Active Jobs',         'stat-counter', 'active_jobs',         'Currently in progress',     '🔧', 0),
    ('service-businesses',     'Estimate Sent',       'stat-counter', 'estimates_sent',      'Quotes this period',        '📋', 1),
    ('service-businesses',     'Estimate Win Rate',   'stat-counter', 'estimate_win_rate',   'Estimates converted to jobs', '✅', 2),
    ('service-businesses',     'Revenue MTD',         'stat-counter', 'revenue_mtd',         'Month-to-date revenue',     '💵', 3),

    -- Recruitment & Staffing
    ('recruitment-staffing',   'Open Positions',      'stat-counter', 'open_positions',      'Active requisitions',       '📌', 0),
    ('recruitment-staffing',   'Candidates Sourced',  'stat-counter', 'candidates_sourced',  'New candidates this period', '👥', 1),
    ('recruitment-staffing',   'Interviews Scheduled','stat-counter', 'interviews_scheduled', 'Upcoming interviews',       '🤝', 2),
    ('recruitment-staffing',   'Placements Made',     'stat-counter', 'placements_made',     'Hired this period',         '✅', 3),

    -- Marketing Agencies
    ('marketing-agencies',     'Active Campaigns',    'stat-counter', 'active_campaigns',    'Running campaigns',         '📣', 0),
    ('marketing-agencies',     'Client Count',        'stat-counter', 'client_count',        'Active clients',            '🏢', 1),
    ('marketing-agencies',     'Content Pieces',      'stat-counter', 'content_pieces',      'Published this period',     '✍️', 2),
    ('marketing-agencies',     'Campaign ROI',        'stat-counter', 'campaign_roi',        'Return on ad spend',        '📊', 3),

    -- Professional Services
    ('professional-services',  'Active Clients',      'stat-counter', 'active_clients',      'Current engagements',       '⚖️', 0),
    ('professional-services',  'Billable Hours',      'stat-counter', 'billable_hours',      'Hours logged this period',  '⏱️', 1),
    ('professional-services',  'Invoices Sent',       'stat-counter', 'invoices_sent',       'Billed this period',        '📄', 2),
    ('professional-services',  'Receivables',         'stat-counter', 'receivables',         'Outstanding payments',      '💳', 3),

    -- Ecommerce & Retail
    ('ecommerce-retail',       'Orders',              'stat-counter', 'orders_count',        'Orders this period',        '🛒', 0),
    ('ecommerce-retail',       'Revenue',             'stat-counter', 'revenue',             'Sales revenue',             '💵', 1),
    ('ecommerce-retail',       'Avg Order Value',     'stat-counter', 'avg_order_value',     'Average ticket size',       '📊', 2),
    ('ecommerce-retail',       'Inventory Alerts',    'stat-counter', 'inventory_alerts',    'Low stock items',           '📦', 3),

    -- Healthcare & Wellness
    ('healthcare-wellness',    'Patients Seen',       'stat-counter', 'patients_seen',       'Appointments completed',    '🏥', 0),
    ('healthcare-wellness',    'Appointments',        'stat-counter', 'appointments',        'Scheduled visits',          '📅', 1),
    ('healthcare-wellness',    'New Patients',        'stat-counter', 'new_patients',        'Intake this period',        '🆕', 2),
    ('healthcare-wellness',    'Client Retention',    'stat-counter', 'client_retention',    'Returning clients %',       '🔄', 3),

    -- Construction & Development
    ('construction-development', 'Active Projects',   'stat-counter', 'active_projects',     'Projects underway',         '🏗️', 0),
    ('construction-development', 'Permits Filed',    'stat-counter', 'permits_filed',       'Submitted this period',     '📋', 1),
    ('construction-development', 'Subcontractors',   'stat-counter', 'subcontractors',      'Active subs on payroll',    '👷', 2),
    ('construction-development', 'Project Budget',   'stat-counter', 'project_budget',      'Total budget tracked',      '💰', 3),

    -- Grant & Funding
    ('grant-funding',          'Grants Found',        'stat-counter', 'grants_found',        'Matching opportunities',    '🔍', 0),
    ('grant-funding',          'Applications',        'stat-counter', 'applications',        'Submitted this period',     '📝', 1),
    ('grant-funding',          'Awarded Amount',      'stat-counter', 'awarded_amount',      'Funding secured',           '💰', 2),
    ('grant-funding',          'Pending Reviews',     'stat-counter', 'pending_reviews',     'Awaiting decision',         '⏳', 3),

    -- Education & Training
    ('education-training',     'Enrolled Students',   'stat-counter', 'enrolled_students',   'Active enrollments',        '📚', 0),
    ('education-training',     'Courses Active',      'stat-counter', 'courses_active',      'Running courses',           '📖', 1),
    ('education-training',     'Completion Rate',     'stat-counter', 'completion_rate',     'Students who finished',     '🎓', 2),
    ('education-training',     'Certificates Issued', 'stat-counter', 'certificates_issued', 'Awarded this period',       '🏅', 3),

    -- Publishing & Media
    ('publishing-media',       'Articles Published',  'stat-counter', 'articles_published',  'Content published',         '📰', 0),
    ('publishing-media',       'Newsletter Subs',     'stat-counter', 'newsletter_subs',     'Subscriber count',          '📧', 1),
    ('publishing-media',       'Avg Read Time',       'stat-counter', 'avg_read_time',       'Minutes per article',       '⏱️', 2),
    ('publishing-media',       'Open Rate',           'stat-counter', 'open_rate',           'Email open %',              '📊', 3),

    -- Site Flipping
    ('site-flipping',          'Sites Acquired',      'stat-counter', 'sites_acquired',      'Websites purchased',        '🛍️', 0),
    ('site-flipping',          'Sites Sold',          'stat-counter', 'sites_sold',          'Sold this period',          '💰', 1),
    ('site-flipping',          'Revenue',             'stat-counter', 'revenue',             'Total sales revenue',       '💵', 2),
    ('site-flipping',          'Profit Margin',       'stat-counter', 'profit_margin',       'Average margin %',          '📊', 3),

    -- Government Contracting
    ('government-contracting', 'Opportunities Found', 'stat-counter', 'opportunities_found', 'Matching solicitations',    '🔍', 0),
    ('government-contracting', 'Proposals Submitted', 'stat-counter', 'proposals_submitted', 'Bids sent this period',     '📝', 1),
    ('government-contracting', 'Win Rate',            'stat-counter', 'win_rate',            'Awarded / submitted %',     '🏆', 2),
    ('government-contracting', 'Contract Value',      'stat-counter', 'contract_value',      'Total awarded value',       '💰', 3)
ON CONFLICT (industry_slug, widget_title) DO NOTHING;
