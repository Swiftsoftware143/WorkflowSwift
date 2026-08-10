-- 020: Ensure industry_slug column exists on tenants (idempotent)
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS industry_slug VARCHAR(100) DEFAULT NULL;
