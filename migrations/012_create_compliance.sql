-- t_d3ed8d72: CREATE INDEX statements made IF NOT EXISTS and the tenant-scoped audit index
-- guarded, so this file is re-run-safe (a filename-keyed ledger can lose its row, e.g. a restore
-- without _migrations). 034_rename_tenant_to_account.sql renames audit_logs.tenant_id -> aid and
-- idx_audit_logs_tenant -> idx_audit_logs_aid, so the old column no longer exists on a
-- live-shaped database; naming it raised 42703. Live skips this file by filename — this edit
-- reaches fresh installs and ledger-loss re-runs only.
CREATE TABLE IF NOT EXISTS compliance_items (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    workflow_type VARCHAR(100) NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    required BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_compliance_items_type ON compliance_items(workflow_type);

CREATE TABLE IF NOT EXISTS compliance_checks (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    compliance_item_id UUID NOT NULL REFERENCES compliance_items(id) ON DELETE CASCADE,
    instance_id UUID NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    checked_by UUID REFERENCES users(id),
    notes TEXT,
    checked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_compliance_checks_instance ON compliance_checks(instance_id);
CREATE INDEX IF NOT EXISTS idx_compliance_checks_status ON compliance_checks(status);

CREATE TABLE IF NOT EXISTS audit_logs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id),
    action VARCHAR(255) NOT NULL,
    entity_type VARCHAR(100),
    entity_id UUID,
    changes JSONB,
    ip_address VARCHAR(45),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $mig$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'audit_logs'
                  AND column_name = 'tenant_id')
       AND to_regclass('public.idx_audit_logs_tenant') IS NULL THEN
        CREATE INDEX idx_audit_logs_tenant ON audit_logs (tenant_id);
    END IF;
END $mig$;
CREATE INDEX IF NOT EXISTS idx_audit_logs_entity ON audit_logs(entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_created ON audit_logs(created_at);
