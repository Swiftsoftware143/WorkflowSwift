-- 062: `checkout_sessions.purchasable_type` learns the plan-purchase value the application itself
-- uses (kanban t_30a0855d).
--
-- THE DEFECT (measured): the plan arm of `POST /api/v1/checkout/create` branches on the literal
-- `"plan"` (src/handlers/checkout_handler.rs:360 — `else if purchasable_type == "plan"`), binds
-- that same string into the INSERT (:487), and `handle_checkout_completed` credits the referring
-- affiliate only when the STORED value is `"plan"` (:1228, `attribute_plan_upgrade`). 041's CHECK
-- admitted ('plan_subscription', 'sponsored_listing', 'ad_zone', 'credits', 'workflow_run') — with
-- no `plan`. Both halves of the contract therefore missed each other:
--
--   * the create path could never insert its own row
--     (`23514 new row for relation "checkout_sessions" violates check constraint
--      "checkout_sessions_purchasable_type_check"`), and
--   * the affiliate-credit compare could never be true, because no row could carry `plan`.
--
-- DECISION — one value, applied in all three places: the plan-purchase value is `plan`.
--   * it is what the create arm recognises (:360) and what it binds (:487);
--   * it is what the affiliate compare reads (:1228);
--   * it is what this app family writes for a plan purchase — ADASwift's checkout_handler.rs:516
--     inserts the literal 'plan' (its table has no CHECK at all, which is why the drift never
--     surfaced there);
--   * `grep -rn 'plan_subscription' src/` -> 0 hits, `select count(*) from checkout_sessions
--     where purchasable_type='plan_subscription'` -> 0, `grep -rn purchasable_type docs/ www*/` ->
--     the admin SPA only *renders* `s.purchasable_type`, and no doc names the value. Renaming the
--     code to the descriptive spelling instead would change the value the admin SPA displays and
--     what the FunnelSwift conversion webhook carries, for a spelling nothing reads.
--
-- WHY ADDITIVE AND NOT A REPLACE: `plan_subscription` stays legal. It is already legal today, no
-- row carries it, no src file writes it, and removing it would turn a request some *external* API
-- caller may already send into a 500 for no benefit. Nothing that was legal becomes illegal and no
-- row is rewritten — the only observable change is that the INSERT the plan arm performs succeeds.
--
-- Applied live as DROP + ADD on a text CHECK (no column change, no row rewritten).
ALTER TABLE checkout_sessions DROP CONSTRAINT IF EXISTS checkout_sessions_purchasable_type_check;
ALTER TABLE checkout_sessions ADD CONSTRAINT checkout_sessions_purchasable_type_check
  CHECK (purchasable_type = ANY (ARRAY['plan'::text, 'plan_subscription'::text,
                                       'sponsored_listing'::text, 'ad_zone'::text,
                                       'credits'::text, 'workflow_run'::text]));
