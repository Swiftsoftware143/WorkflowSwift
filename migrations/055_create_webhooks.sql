-- 055_create_webhooks.sql
-- Real per-account outbound webhook registry for WorkflowSwift.
--
-- Same defect family as 054_create_tag_groups.sql (kanban t_01fa9bbc): the auto-generated
-- webhooks handler read a 'webhooks' relation that no migration ever created, was not
-- tenant-scoped, and swallowed the missing-relation error, so GET answered an empty 200
-- while POST/PUT/DELETE answered 500. The admin shell ships the screen
-- (Communications -> Webhooks) and posts name + url + is_active, so all three live here.
--
-- This table is the TENANT-SUPPLIED registry of endpoints. It is deliberately NOT the
-- inbound payment webhook log (payment_webhook_events) and NOT the public
-- /api/v1/webhooks/stripe|paypal receivers, which stay in the public router.
--
-- Idempotent: safe to re-run.
CREATE TABLE IF NOT EXISTS webhooks (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    aid         UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    url         TEXT NOT NULL,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_webhooks_aid ON webhooks(aid);
