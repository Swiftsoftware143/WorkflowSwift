-- 118_plan_tier_monthly_credits.sql
--
-- NO PLAN EVER GRANTED A CREDIT (kanban t_b0774174, measured 2026-11-18).
--
-- `signup::create_account` grants a new workspace `plan_tiers.features->>'credits_monthly'`
-- credits when its tier declares one — and NO tier declared one:
--   select slug, features->>'credits_monthly' -> free|NULL, starter|NULL, professional|NULL,
--                                                enterprise|NULL
-- so the grant was dead code on every door. The Run gate
-- (`workflow_handler::run_in_process`) refuses whenever `SUM(amount)` from `credit_transactions`
-- is below 1, so NO workspace in this install could run a single workflow: 12 of 13 accounts had
-- no credit row at all, including every Enterprise workspace, and the only way to fund one was an
-- operator hand-inserting credits after a "request a credit pack" ticket.
--
-- The public pricing page already advertises the allowance (`www/pricing.html` renders
-- `credits_monthly` from `/api/v1/public/plans`) and the admin guide already documents it
-- ("each plan tier carries its own monthly credit allowance ... a new workspace is granted that
-- many credits by its signup"). This file makes the catalogue match the promise.
--
-- THE NUMBERS are ALLOWANCE DEFAULTS, not prices: credits have no self-serve price in this app
-- (the Credits screen explicitly says "nothing is charged in the app"). They are anchored on the
-- two things the install already carries — the one 50-credit grant this database ever held
-- ("Monthly Test WF Regular plan credit grant") and the pack ladder (100 / 500 / 2000 / 10000) —
-- and they scale with the workflow ceiling each tier advertises:
--     free          3 workflows  ->    50 credits
--     starter      15 workflows  ->   250 credits
--     professional unlimited     ->  1000 credits
--     enterprise   unlimited     ->  5000 credits
-- They are deliberately operator-editable: the Admin -> Plans editor writes this same
-- `features.credits_monthly` key, so the seeded value is only a default. The guard below never
-- overwrites a value the operator has already set.
--
-- BACKFILL: every existing account that has NEVER had a credit row is granted its resolved plan's
-- allowance, because those workspaces are the ones the dead grant path left unable to run.
-- Accounts that have any ledger row are untouched — a workspace that spent its credits down to
-- zero is not re-credited (only an empty ledger, i.e. a workspace the grant never reached).
--
-- Idempotent by construction (this app's runner re-executes any file it could not record):
--   * seat:    `NOT (features ? 'credits_monthly')` — a replay finds the key present and writes
--              nothing; an operator's number always wins.
--   * backfill: `NOT EXISTS (... credit_transactions ...)` — a replay finds the row it inserted.
--
-- Reversal (one batch): delete the backfill rows by their `reference_id`, then drop the keys:
--   DELETE FROM credit_transactions WHERE reference_id = 'migration_118';
--   UPDATE plan_tiers SET features = features - 'credits_monthly'
--    WHERE slug IN ('free','starter','professional','enterprise');
-- (Rows the signup granted afterwards are NOT removed by either statement.)

BEGIN;

-- 1. The catalogue declares the allowance. `jsonb_build_object` stores a JSON NUMBER, which is
--    what `(features->>'credits_monthly')::integer` reads on every grant path.
UPDATE plan_tiers
   SET features = features || jsonb_build_object('credits_monthly', 50)
 WHERE slug = 'free' AND NOT (features ? 'credits_monthly');

UPDATE plan_tiers
   SET features = features || jsonb_build_object('credits_monthly', 250)
 WHERE slug = 'starter' AND NOT (features ? 'credits_monthly');

UPDATE plan_tiers
   SET features = features || jsonb_build_object('credits_monthly', 1000)
 WHERE slug = 'professional' AND NOT (features ? 'credits_monthly');

UPDATE plan_tiers
   SET features = features || jsonb_build_object('credits_monthly', 5000)
 WHERE slug = 'enterprise' AND NOT (features ? 'credits_monthly');

-- 2. Existing workspaces the dead grant path left at zero get their plan's allowance.
--    Plan resolution follows `features::resolve_plan_id`: the newest ACTIVE `account_plans` row,
--    then `accounts.plan_id`, then the cheapest active tier (what the resolver calls this app's
--    default). The ledger guard is what makes this "a workspace that never had credits" rather
--    than "a workspace with a low balance".
WITH resolved AS (
    SELECT a.id AS aid,
           COALESCE(
               (SELECT x.plan_id
                  FROM account_plans x
                 WHERE x.aid = a.id AND x.status = 'active'
                 ORDER BY x.started_at DESC NULLS LAST
                 LIMIT 1),
               a.plan_id,
               (SELECT p.id
                  FROM plan_tiers p
                 WHERE p.is_active = true
                 ORDER BY p.price_monthly ASC NULLS LAST, p.sort_order ASC NULLS LAST
                 LIMIT 1)
           ) AS plan_id
      FROM accounts a
)
INSERT INTO credit_transactions
       (id, aid, amount, transaction_type, description, reference_id)
SELECT gen_random_uuid(),
       r.aid,
       (pt.features->>'credits_monthly')::integer,
       'grant',
       'Welcome credits: first month of ' || COALESCE(pt.name, 'your plan'),
       'migration_118'
  FROM resolved r
  JOIN plan_tiers pt ON pt.id = r.plan_id
 WHERE (pt.features->>'credits_monthly') ~ '^[0-9]+$'
   AND (pt.features->>'credits_monthly')::integer > 0
   AND NOT EXISTS (
       SELECT 1 FROM credit_transactions ct WHERE ct.aid = r.aid
   );

COMMENT ON COLUMN credit_transactions.billing_month IS
  'Reserved: no reader or writer today. Monthly allowance renewal is NOT implemented — the plan allowance is granted ONCE, when a workspace first lands on a plan (signup, admin plan assign, plan-request resolve, portfolio sync; migration 118 backfilled pre-existing workspaces).';

COMMIT;
