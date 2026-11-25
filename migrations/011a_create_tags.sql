-- t_d3ed8d72: renamed 011_create_tags.sql -> 011a_create_tags.sql so the 011 group stops sharing
-- one version prefix (Gate 1b). The suffix letter keeps the file in its ORIGINAL byte-order slot
-- ('_' 0x5F < 'a' 0x61 < '2'), so a from-zero build runs it in the same place. A rename also
-- re-runs the file once on an existing database, so every statement below is re-run-safe:
-- CREATE TABLE IF NOT EXISTS, IF NOT EXISTS on the two indexes whose columns survive, and a
-- guarded CREATE INDEX for idx_tags_tenant, whose column 034_rename_tenant_to_account.sql renames
-- tenant_id -> aid (naming the old column raised 42703 and aborted the boot on a live copy).
CREATE TABLE IF NOT EXISTS tags (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(100) NOT NULL,
    color VARCHAR(7),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, name)
);

DO $mig$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'tags'
                  AND column_name = 'tenant_id')
       AND to_regclass('public.idx_tags_tenant') IS NULL THEN
        CREATE INDEX idx_tags_tenant ON tags (tenant_id);
    END IF;
END $mig$;

CREATE TABLE IF NOT EXISTS tag_assignments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tag_id UUID NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    entity_type VARCHAR(100) NOT NULL,
    entity_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tag_id, entity_type, entity_id)
);

CREATE INDEX IF NOT EXISTS idx_tag_assignments_tag ON tag_assignments(tag_id);
CREATE INDEX IF NOT EXISTS idx_tag_assignments_entity ON tag_assignments(entity_type, entity_id);
