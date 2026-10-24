-- Credit-pack requests (cron 2026-10-24).
--
-- WHY: the Credits screen listed four credit packs with their prices and gave the customer no way
-- to obtain any of them. The only purchase route (`POST /credits/package`) is super-admin-only and
-- acts on the CALLER's own account, so nobody could buy credits from the product; meanwhile a
-- deployed workflow's balance gate needs at least one credit, and a new workspace has none. The
-- result was a hard dead end: "Insufficient credits. Need 2, have 0. Purchase more credits." on a
-- screen with no purchase.
--
-- A request is a TICKET, exactly like a plan upgrade: nothing is charged when it is filed, the
-- operator sees it in the Plan requests queue and the same resolve that applies a plan also applies
-- the pack's credits (see `admin_update_status`).
--
-- Both columns are additive and nullable, so an older binary reading this table is unaffected.
-- Idempotent: this app re-runs every file in ./migrations at boot (src/db.rs), so each statement
-- must be safe on a database that already has it.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS credit_pack_id uuid;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS credits_applied_at timestamptz;
