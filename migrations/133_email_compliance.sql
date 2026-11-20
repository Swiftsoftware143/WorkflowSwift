-- Email compliance: the opt-out, the suppression list, and the evidence.
--
-- Why this exists here (2026-11-20): this app sends mail to people who have no account with us —
-- `workflow_notify` tells whoever a workflow names that something happened, and the only way out was
-- to reply and hope. A notification mail with no working opt-out is bulk mail without an opt-out, and
-- one complaint from a shared sending identity damages the reputation that carries every customer's
-- receipts.
--
-- Shapes match CoreSwift's and IncentiveSwift's tables on purpose (`email_suppressions`,
-- `email_message_log`, `email_consent`), so the fleet's compliance layers behave identically and a
-- report written for one app is true of the others.
--
-- SCOPE: this app mails as ITSELF — one brand, one sending identity (no-reply@mail.workflowswift.com) —
-- so its do-not-mail list is the app's, not one workspace's, and it is recorded against the fixed
-- `PLATFORM_TENANT` id (see `src/compliance.rs`). The tenant column stays in the shape so the day an
-- account sends under its own identity the same code keys per account with no migration.
--
-- Additive only; re-run at every boot is safe (this app's runner replays unrecorded files).
CREATE TABLE IF NOT EXISTS email_suppressions (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL,
    address     text NOT NULL,
    -- 'all' stops everything except transactional; 'transactional' is written only by a hard bounce or
    -- a spam complaint, where delivering at all is the harm.
    mail_class  text NOT NULL DEFAULT 'all' CHECK (mail_class IN ('all', 'bulk', 'notification', 'transactional')),
    reason      text NOT NULL CHECK (reason IN ('unsubscribed', 'hard_bounce', 'complaint', 'manual', 'provider')),
    source      text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, address, mail_class)
);

CREATE INDEX IF NOT EXISTS idx_wf_email_suppressions_lookup
    ON email_suppressions (tenant_id, address);

CREATE TABLE IF NOT EXISTS email_message_log (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id             uuid NOT NULL,
    mail_class            text NOT NULL CHECK (mail_class IN ('bulk', 'notification', 'transactional')),
    address               text NOT NULL,
    subject               text,
    provider              text,
    provider_message_id   text,
    status                text NOT NULL CHECK (status IN ('sent', 'failed', 'refused_suppressed', 'refused_quota')),
    detail                text,
    created_at            timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_wf_email_message_log_tenant
    ON email_message_log (tenant_id, created_at DESC);

CREATE TABLE IF NOT EXISTS email_consent (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL,
    address     text NOT NULL,
    mail_class  text NOT NULL DEFAULT 'bulk',
    state       text NOT NULL CHECK (state IN ('granted', 'revoked')),
    how         text,
    ip          text,
    user_agent  text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_wf_email_consent_tenant
    ON email_consent (tenant_id, address, created_at DESC);

-- Retention, not a write amplification: the operator's list reads the newest 1000 rows.
COMMENT ON TABLE email_suppressions IS 'Addresses that asked this app to stop mailing them (per class; transactional exempt by law).';
COMMENT ON TABLE email_message_log IS 'One row per send attempt, including refused_suppressed — the evidence a suppression did its job.';
