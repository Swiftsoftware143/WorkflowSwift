-- 034a: Agent tables — RENAMED from `012_agent_tables.sql` (card t_4ebd6f98).
--
-- WHY THE RENAME
--   Both FKs here point at `accounts(id)`, and `accounts` is created by
--   034_rename_tenant_to_account.sql (it is the rename target of `tenants`), so as `012_*` this file
--   ran 22 files too early and a from-zero build died with `ERROR: relation "accounts" does not
--   exist`. It also FKs `portfolio_companies` (015). `034a` runs immediately after 034, where both
--   relations exist.
--
-- PRODUCTION SAFETY (applied once on the next boot; the ledger keeps the old name): every statement
-- is `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`, and production has had all six
-- tables and indexes since 2026-08-09, so the file is a no-op there.
--
-- Agent profiles (one per workspace)
CREATE TABLE IF NOT EXISTS agent_profiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    aid UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    portfolio_company_id UUID REFERENCES portfolio_companies(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'worker',
    model TEXT,
    budget_credits INTEGER DEFAULT 0,
    credits_spent INTEGER DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'idle' CHECK (status IN ('idle','running','paused','disabled')),
    config JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Agent tickets (kanban items)
CREATE TABLE IF NOT EXISTS agent_tickets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id UUID REFERENCES agent_profiles(id) ON DELETE CASCADE,
    aid UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    portfolio_company_id UUID REFERENCES portfolio_companies(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'backlog' CHECK (status IN ('backlog','todo','in_progress','review','done','archived')),
    priority TEXT NOT NULL DEFAULT 'medium' CHECK (priority IN ('low','medium','high','critical')),
    assigned_to TEXT,
    source TEXT DEFAULT 'manual',
    source_reference TEXT,
    budget_credits INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Ticket steps (activity log within a ticket)
CREATE TABLE IF NOT EXISTS ticket_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ticket_id UUID NOT NULL REFERENCES agent_tickets(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    description TEXT,
    actor TEXT DEFAULT 'system',
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_agent_profiles_aid ON agent_profiles(aid);
CREATE INDEX IF NOT EXISTS idx_agent_profiles_workspace ON agent_profiles(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_agent_tickets_aid ON agent_tickets(aid);
CREATE INDEX IF NOT EXISTS idx_agent_tickets_workspace ON agent_tickets(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_agent_tickets_status ON agent_tickets(status);
CREATE INDEX IF NOT EXISTS idx_ticket_steps_ticket ON ticket_steps(ticket_id);
