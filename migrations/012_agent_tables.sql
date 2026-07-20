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
