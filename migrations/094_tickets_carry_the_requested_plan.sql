-- 094 — a plan request now carries the plan it asks for, and records when the switch was made.
--
-- WHY (cron 2026-10-20, measured live): resolving a plan request only moved its STATUS. The customer
-- asked for a paid tier, the operator pressed "Resolved", the customer was mailed "handled" — and the
-- workspace stayed on the free plan, because the tier existed only as words inside `subject`
-- ("Upgrade request: Professional plan"). Before-fix measurement: filed a request for Professional,
-- resolved it as a super-admin, /auth/me still answered plan_name "Free" and the only active
-- `account_plans` row was `free`.
--
-- `plan_slug` is the catalogue slug the customer asked for; the handler validates it against
-- `plan_tiers` on the way in, so an unknown tier is a 400 at filing time instead of a request nobody
-- can match. `plan_applied_at` is when the operator's resolve actually moved the workspace: NULL means
-- "not applied yet", which is what makes a second resolve idempotent instead of superseding the same
-- tier twice.
--
-- This app's runner re-reads ./migrations on every boot and applies the files it has not recorded, so
-- every statement here is idempotent and safe to re-run.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS plan_slug text;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS plan_applied_at timestamptz;

COMMENT ON COLUMN tickets.plan_slug IS
  'plan_tiers.slug the request asks for (cron 2026-10-20). NULL for an ordinary support ticket.';
COMMENT ON COLUMN tickets.plan_applied_at IS
  'When the operator''s resolve moved the workspace onto plan_slug. NULL = not applied yet.';
