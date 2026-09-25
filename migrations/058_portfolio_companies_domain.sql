-- 058_portfolio_companies_domain.sql
-- `portfolio_companies.domain` — the company's own web domain (kanban t_b9c74751).
--
-- The internal sync contract has carried `domain` since the fleet's portfolio-sync endpoints
-- existed: the POST /api/v1/internal/portfolio-companies handler in this crate reads it from the
-- body (src/handlers/portfolio_handler.rs::internal_create_portfolio_company) and binds it as $7,
-- and ADASwift's twin endpoint stores the same field on `tenants.domain` / `clients.domain`.
-- WorkflowSwift's table simply never had the column, so the statement was a guaranteed
-- ERROR 42703 — the sync from every sister app failed on it (and on `account_slug`, which this
-- file does not add: it is not a portfolio_companies column either and the row's `aid` already
-- identifies the account).
--
-- Additive and idempotent. Nullable with the same shape as the sibling text columns
-- (`email`, `description`), so existing rows read back as '' rather than NULL.
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS domain text DEFAULT '';

COMMENT ON COLUMN portfolio_companies.domain IS
    'Company web domain, from the internal portfolio-sync contract (migrations/058).';
