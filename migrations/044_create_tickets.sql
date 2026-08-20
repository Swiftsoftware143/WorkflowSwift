-- 044_create_tickets.sql
-- Real ticket/support system for WorkflowSwift.
-- Replaces the auto-generated tickets stub (which targeted a non-existent
-- 'tickets' table and was not auth/tenant-scoped).
--
-- Idempotent: safe to re-run.
CREATE TABLE IF NOT EXISTS tickets (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    aid         UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    subject     TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'open',
    priority    TEXT NOT NULL DEFAULT 'medium',
    source      TEXT NOT NULL DEFAULT 'manual',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_tickets_aid ON tickets(aid);
CREATE INDEX IF NOT EXISTS idx_tickets_status ON tickets(status);
