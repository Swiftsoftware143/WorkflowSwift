-- kanban t_d3ff37ef — the Notify step gains REAL email + SMS channels.
--
-- The parent decision (t_7e0babd6, David 2026-10-04): "The notify step should have email and
-- SMS so it should be able to be set up for either or both." The retired arms (t_08be842f)
-- existed because nothing in this app could deliver either channel; this migration creates the
-- missing pieces. Everything below is FAIL-CLOSED: a channel is only offerable once a sender
-- exists, and a channel that is offered always has a destination the account actually owns.
--
-- 1. A PHONE on the account's own people. This is the ONLY SMS destination this product allows:
--    a Notify step names WHICH of the account's people to reach, never a free-text number, so
--    the platform is never an open outbound relay.
ALTER TABLE users ADD COLUMN IF NOT EXISTS phone text;

-- The shape check is deliberately LOOSER than the Rust validator (which enforces
-- ^\+[1-9][0-9]{7,14}$ before a write), so the database can never refuse a value the
-- application accepted. Guarded because ADD CONSTRAINT has no IF NOT EXISTS.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'users_phone_format_check'
    ) THEN
        ALTER TABLE users ADD CONSTRAINT users_phone_format_check
            CHECK (phone IS NULL OR phone ~ '^\+?[0-9][0-9 ()-]{6,24}$');
    END IF;
END $$;

-- 2. The outbound notify record. It is BOTH the audit trail and the throttle counter: every
--    dispatch attempt lands here with its outcome (sent / failed / refused / throttled), so a
--    throttled or refused send can never be reported as a silent `completed`.
CREATE TABLE IF NOT EXISTS notify_send_attempts (
    id           uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    aid          uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    workflow_id  uuid,
    step_index   integer,
    channel      varchar(20) NOT NULL,
    recipient    text NOT NULL DEFAULT '',
    status       varchar(20) NOT NULL,
    detail       text,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_notify_attempts_aid_created
    ON notify_send_attempts (aid, created_at DESC);

-- 3. The SMS sender. The SAME shape/encryption discipline as `admin_settings.email`
--    (kanban t_a794cb09): `api_key` / `api_secret` are sealed with the app's `enc:v1:`
--    envelope before they are stored, and an empty provider means "not configured"
--    (fail-closed: an `sms` Notify step is refused at the write path until this is filled in
--    from Admin > Settings > SMS).
INSERT INTO admin_settings (key, value, description)
VALUES (
    'sms',
    '{"provider":"","api_key":"","api_secret":"","account_sid":"","from_number":"","api_url":""}'::jsonb,
    'SMS provider (kanban t_d3ff37ef)'
)
ON CONFLICT (key) DO NOTHING;

-- 4. The per-plan outbound notify cap — the throttle knob, beside the plan's other caps
--    (max_workflows / max_users / retention_days). -1 = unlimited (the same convention
--    plan_tiers already uses for max_workflows/max_users); NULL = unset and falls back to
--    admin_settings.limits.notify_per_hour, then to the compiled constant.
ALTER TABLE plan_tiers ADD COLUMN IF NOT EXISTS max_notify_per_hour integer;

UPDATE plan_tiers
   SET max_notify_per_hour = CASE slug
        WHEN 'free'         THEN 50
        WHEN 'starter'      THEN 200
        WHEN 'professional' THEN 1000
        WHEN 'enterprise'   THEN -1
        ELSE 200
   END
 WHERE max_notify_per_hour IS NULL;

-- 5. The install-wide fallback, in the existing Limits settings block (panel-manageable).
UPDATE admin_settings
   SET value = value || '{"notify_per_hour":200}'::jsonb,
       updated_at = NOW()
 WHERE key = 'limits'
   AND NOT (value ? 'notify_per_hour');
