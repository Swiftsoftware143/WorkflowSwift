CREATE TABLE IF NOT EXISTS plan_tiers (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(100) UNIQUE NOT NULL,
    description TEXT,
    price_monthly NUMERIC(10,2),
    price_yearly NUMERIC(10,2),
    features JSONB DEFAULT '{}'::jsonb,
    checkout_url TEXT,
    is_active BOOLEAN NOT NULL DEFAULT true,
    sort_order INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Columns production has and no migration created (card t_4ebd6f98, read from its catalog).
    -- 037_admin_settings_and_retention.sql adds max_workflows/max_users/retention_days and the three
    -- can_*/has_* flags; these six are the admin Plans UI's remaining fields, and
    -- 047_seed_plan_limits.sql reads the table only after 037 runs.
    icon TEXT,
    payment_provider TEXT,
    limit_value INTEGER,
    category_id UUID,
    label TEXT,
    plan_id UUID
);

CREATE INDEX idx_plan_tiers_active ON plan_tiers(is_active);
CREATE INDEX idx_plan_tiers_sort ON plan_tiers(sort_order);

CREATE TABLE IF NOT EXISTS tenant_plans (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    plan_id UUID NOT NULL REFERENCES plan_tiers(id),
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Columns production has and no migration created (card t_4ebd6f98). Both survive the
    -- 034 rename to `account_plans`, and 057_account_plans_one_active_row.sql reads `is_active`
    -- (its partial unique index is `... WHERE is_active`), so without it a from-zero build ends
    -- with `column "is_active" does not exist` and refuses the boot.
    sort_order INTEGER DEFAULT 0,
    is_active BOOLEAN DEFAULT true
);

CREATE INDEX idx_tenant_plans_tenant ON tenant_plans(tenant_id);
CREATE INDEX idx_tenant_plans_status ON tenant_plans(status);

CREATE TABLE IF NOT EXISTS invoices (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    plan_id UUID NOT NULL REFERENCES plan_tiers(id),
    amount NUMERIC(10,2) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    due_date TIMESTAMPTZ,
    paid_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_invoices_tenant ON invoices(tenant_id);
CREATE INDEX idx_invoices_status ON invoices(status);
