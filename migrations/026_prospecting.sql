CREATE TABLE IF NOT EXISTS prospecting_leads (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    business_name VARCHAR(255) NOT NULL,
    website TEXT DEFAULT NULL,
    industry VARCHAR(255) DEFAULT NULL,
    location VARCHAR(255) DEFAULT NULL,
    estimated_size VARCHAR(50) DEFAULT NULL,
    social_links TEXT[] DEFAULT NULL,
    contact_email VARCHAR(255) DEFAULT NULL,
    contact_phone VARCHAR(50) DEFAULT NULL,
    enriched BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_prospecting_leads_tenant_id ON prospecting_leads(tenant_id);
