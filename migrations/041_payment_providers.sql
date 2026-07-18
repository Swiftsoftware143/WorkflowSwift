-- Payment Provider Integration System
-- Enables admin to configure Stripe, PayPal, Square, or Paddle
-- via the admin panel without requiring developer intervention for key rotation.

-- Drop tables if they exist for idempotency
DROP TABLE IF EXISTS payment_webhook_events CASCADE;
DROP TABLE IF EXISTS checkout_sessions CASCADE;
DROP TABLE IF EXISTS payment_providers CASCADE;

CREATE TABLE payment_providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_type TEXT NOT NULL CHECK (provider_type IN ('stripe', 'paypal', 'square', 'paddle')),
    label TEXT NOT NULL DEFAULT '',
    is_active BOOLEAN NOT NULL DEFAULT false,
    -- Encrypted API credentials (encrypted-at-rest via app-layer encryption)
    api_key_encrypted TEXT,
    webhook_secret_encrypted TEXT,
    -- Provider-specific config stored as JSON
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- For Stripe: publishable key (stored in plaintext for frontend use)
    publishable_key TEXT,
    -- Webhook endpoint path that this provider sends events to
    webhook_path TEXT,
    -- Test mode flag
    is_test_mode BOOLEAN NOT NULL DEFAULT true,
    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Only one provider can be active at a time per type
    UNIQUE(provider_type)
);

CREATE INDEX IF NOT EXISTS idx_payment_providers_active ON payment_providers(is_active) WHERE is_active = true;

-- Checkout sessions table for tracking in-flight payments
CREATE TABLE checkout_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    provider_type TEXT NOT NULL,
    provider_session_id TEXT,
    -- What's being purchased
    purchasable_type TEXT NOT NULL CHECK (purchasable_type IN ('plan_subscription', 'sponsored_listing', 'ad_zone', 'credits', 'workflow_run')),
    purchasable_id UUID,
    -- Pricing
    amount NUMERIC(10,2) NOT NULL,
    currency TEXT NOT NULL DEFAULT 'USD',
    -- Status tracking
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed', 'failed', 'expired', 'refunded')),
    -- Webhook verification
    webhook_received_at TIMESTAMPTZ,
    webhook_event_id TEXT,
    -- Metadata
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_checkout_sessions_account ON checkout_sessions(account_id);
CREATE INDEX IF NOT EXISTS idx_checkout_sessions_status ON checkout_sessions(status);
CREATE INDEX IF NOT EXISTS idx_checkout_sessions_provider ON checkout_sessions(provider_session_id);

-- Provider event log (for debugging webhooks)
CREATE TABLE payment_webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_type TEXT NOT NULL,
    event_type TEXT,
    event_id TEXT,
    raw_body JSONB,
    headers JSONB,
    status TEXT NOT NULL DEFAULT 'received' CHECK (status IN ('received', 'processed', 'failed', 'ignored')),
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_payment_webhook_events_provider ON payment_webhook_events(provider_type);
CREATE INDEX IF NOT EXISTS idx_payment_webhook_events_status ON payment_webhook_events(status);
