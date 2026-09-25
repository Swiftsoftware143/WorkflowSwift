CREATE TABLE IF NOT EXISTS workflow_templates (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    description TEXT,
    category VARCHAR(100) NOT NULL,
    tags JSONB DEFAULT '[]'::jsonb,
    is_public BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Pre-034 column set (card t_4ebd6f98). `tenant_id` is renamed to `aid` by
    -- 034_rename_tenant_to_account.sql (measured: no other file ever adds it, so without it that
    -- file dies with `column "tenant_id" does not exist` and the whole cascade behind it, including
    -- every `accounts`-referencing file, never runs). The remaining columns exist in production
    -- (read from its catalog) and are created by NO migration at all: `category_id` is the second
    -- half of the industry/template drift 061 repoints, `icon`/`sort_order`/`plan_id`/`is_active`
    -- are written by the admin template UI.
    tenant_id UUID,
    category_id UUID,
    icon TEXT,
    is_active BOOLEAN DEFAULT true,
    sort_order INTEGER DEFAULT 0,
    plan_id UUID
);

CREATE INDEX idx_workflow_templates_category ON workflow_templates(category);
CREATE INDEX idx_workflow_templates_public ON workflow_templates(is_public);

CREATE TABLE IF NOT EXISTS workflow_template_steps (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    template_id UUID NOT NULL REFERENCES workflow_templates(id) ON DELETE CASCADE,
    step_type VARCHAR(50) NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    config JSONB DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_template_steps_template ON workflow_template_steps(template_id);
CREATE INDEX idx_template_steps_sort ON workflow_template_steps(template_id, sort_order);
