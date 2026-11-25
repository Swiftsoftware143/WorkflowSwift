-- 136_agent_parity_archived_at.sql
-- Agent parity (card t_8df5d174): leads and tickets carry no is_active column, so their archive
-- tools use the replay-safe archived_at pattern instead — soft, audited, and the row stays
-- readable to the tenant's own read tools and export paths. archived_at IS NULL means live;
-- a non-null value is the archive. Nothing here hard-deletes.
ALTER TABLE leads ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;
CREATE INDEX IF NOT EXISTS idx_leads_archived_at ON leads(archived_at) WHERE archived_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tickets_archived_at ON tickets(archived_at) WHERE archived_at IS NOT NULL;
