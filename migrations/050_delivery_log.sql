-- 050_delivery_log.sql
-- t_b505bce2: POST /api/v1/integration-dispatch returned 500 on every request.
--
-- src/security/webhook_security.rs::check_daily_limit counted rows in `delivery_log` to enforce
-- integration_targets.daily_limit, but no migration ever created that table, so the count query
-- failed with `relation "delivery_log" does not exist` and the handler mapped it to a 500.
--
-- This creates the table the daily-limit guard was written against. One row per outbound webhook
-- ATTEMPT (allowed by the guard, then fired), so COUNT(*) for a target today is the number of
-- deliveries already made and the cap can be enforced.
CREATE TABLE IF NOT EXISTS delivery_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- ON DELETE CASCADE: retiring a target retires its delivery history (and its quota).
    target_id UUID REFERENCES integration_targets(id) ON DELETE CASCADE,
    aid UUID REFERENCES accounts(id) ON DELETE CASCADE,
    -- The webhook URL the attempt was made to, kept for audit even if the target is retargeted.
    target TEXT NOT NULL,
    -- 'success' (2xx), 'rejected' (non-2xx response), 'failed' (transport error).
    outcome TEXT NOT NULL DEFAULT 'attempted',
    status_code INTEGER,
    error_message TEXT,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The guard's hot path: count this target's attempts since midnight UTC.
CREATE INDEX IF NOT EXISTS idx_delivery_log_target_attempted
    ON delivery_log(target_id, attempted_at);
-- Audit path: everything a tenant fired today.
CREATE INDEX IF NOT EXISTS idx_delivery_log_aid_attempted
    ON delivery_log(aid, attempted_at);
