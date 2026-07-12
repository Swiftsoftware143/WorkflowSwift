CREATE TABLE IF NOT EXISTS competitors (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    website TEXT DEFAULT NULL,
    description TEXT DEFAULT NULL,
    strengths TEXT[] DEFAULT NULL,
    weaknesses TEXT[] DEFAULT NULL,
    market_share DOUBLE PRECISION DEFAULT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_checked_at TIMESTAMPTZ DEFAULT NULL
);

CREATE INDEX IF NOT EXISTS idx_competitors_tenant_id ON competitors(tenant_id);
