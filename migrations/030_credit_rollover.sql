-- Migration: 030_credit_rollover
-- Adds credit rollover support so unused credits persist month-to-month
-- Also adds a transaction_type for rollover entries

-- Add rollover_balance column to credit_transactions for tracking
ALTER TABLE credit_transactions ADD COLUMN IF NOT EXISTS expired_at TIMESTAMPTZ;

-- Create a view for available balance (excludes expired)
CREATE OR REPLACE VIEW credit_balance AS
SELECT
    tenant_id,
    COALESCE(SUM(amount), 0) AS balance
FROM credit_transactions
WHERE (expired_at IS NULL OR expired_at > NOW())
GROUP BY tenant_id;

-- Track which month each transaction belongs to
ALTER TABLE credit_transactions ADD COLUMN IF NOT EXISTS billing_month DATE;
CREATE INDEX IF NOT EXISTS idx_credit_transactions_billing ON credit_transactions(billing_month);

-- Credit packages table (unchanged, for reference)
-- CREATE TABLE IF NOT EXISTS credit_packages ( ... );
