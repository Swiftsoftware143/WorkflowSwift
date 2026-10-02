-- 068: close the orphaned-child class on `aid` (kanban t_d7916c1d).
--
-- WHY
--   Measured 2026-10-02: 241 rows in 5 tables pointed `aid` at an `accounts` row that no longer
--   existed, and NONE of those 5 columns carried a foreign key — so the account-delete paths
--   (bin/ws-residue-cleanup.sh, the fleet probe sweep) deleted the parent and left the children
--   behind, and nothing ever removed them. 238 of the 241 were machine/probe residue, attributed
--   row-by-row from their own content (the parent was gone, so the parent cannot attribute them) and
--   retired with a backup + in-transaction fingerprint: /opt/swift/audits/t_d7916c1d/.
--   This file (a) retires any residual of that class on an instance that still carries it and
--   (b) ARMS the foreign key so a parent delete can never leave one again.
--
-- WHAT IS NOT HERE, ON PURPOSE
--   `email_templates.aid` gets NO foreign key. Its 3 "orphans" are the app's own system templates on
--   the deliberate nil-UUID sentinel (migrations/012, `let aid = Uuid::nil()` in
--   src/handlers/email_templates_handler.rs and src/handlers/admin_settings_handler.rs), not deleted-
--   parent residue, and both halves of the cure are blocked by measurement:
--     * arming the FK as-is raises 23503 (Key (aid)=(00000000-...) is not present in accounts);
--     * normalising the sentinel to NULL raises 23505 on idx_email_templates_unique, because there are
--       two `welcome` is_default=true rows (404dbfc7..., aid NULL, 2026-08-08 and 00000000-...-0001,
--       aid nil, 2026-08-11). Choosing which default to drop is a product decision, not this card's.
--   Evidence: /opt/swift/audits/t_d7916c1d/11-email-templates-sentinel.txt
--
-- IDEMPOTENT BY CONSTRUCTION
--   Applied to a database that is already clean and already constrained (the live instance is, the
--   constraints were armed on 2026-10-02 as part of this card) this file retires 0 rows and arms 0
--   constraints, and says so in the boot log. It is therefore safe to leave for the next boot.

-- ── 1. retire any residual orphan of the class (children first; counted in the boot log) ──────────
DO $retire$
DECLARE n bigint; total bigint := 0;
BEGIN
    -- a doomed template's steps first: on an instance that has not armed workflow_templates_aid_fkey
    -- yet there is no cascade to take them, and the orphan audit would still see them.
    DELETE FROM workflow_template_steps s
     WHERE s.template_id IN (SELECT t.id FROM workflow_templates t
                              WHERE t.aid IS NOT NULL
                                AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = t.aid));
    GET DIAGNOSTICS n = ROW_COUNT; total := total + n;
    RAISE NOTICE 't_d7916c1d: retired % orphaned workflow_template_steps rows', n;

    DELETE FROM leads c
     WHERE c.aid IS NOT NULL AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = c.aid);
    GET DIAGNOSTICS n = ROW_COUNT; total := total + n;
    RAISE NOTICE 't_d7916c1d: retired % orphaned leads rows', n;

    DELETE FROM workflow_templates c
     WHERE c.aid IS NOT NULL AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = c.aid);
    GET DIAGNOSTICS n = ROW_COUNT; total := total + n;
    RAISE NOTICE 't_d7916c1d: retired % orphaned workflow_templates rows', n;

    DELETE FROM surfaces c
     WHERE c.aid IS NOT NULL AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = c.aid);
    GET DIAGNOSTICS n = ROW_COUNT; total := total + n;
    RAISE NOTICE 't_d7916c1d: retired % orphaned surfaces rows', n;

    DELETE FROM user_api_keys c
     WHERE c.aid IS NOT NULL AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = c.aid);
    GET DIAGNOSTICS n = ROW_COUNT; total := total + n;
    RAISE NOTICE 't_d7916c1d: retired % orphaned user_api_keys rows', n;

    RAISE NOTICE 't_d7916c1d: retired % orphaned child rows in total', total;
END
$retire$;

-- ── 2. arm the FK on every column the class was measured on (all idempotent) ──────────────────────
DO $arm$
DECLARE t text; n integer := 0;
BEGIN
    FOREACH t IN ARRAY ARRAY['leads', 'workflow_templates', 'surfaces', 'user_api_keys'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint
                        WHERE conname = t || '_aid_fkey' AND conrelid = ('public.' || t)::regclass) THEN
            EXECUTE format('ALTER TABLE public.%I ADD CONSTRAINT %I FOREIGN KEY (aid) '
                           'REFERENCES public.accounts(id) ON DELETE CASCADE', t, t || '_aid_fkey');
            n := n + 1;
            RAISE NOTICE 't_d7916c1d: armed %.aid -> accounts(id) ON DELETE CASCADE', t;
        END IF;
    END LOOP;
    RAISE NOTICE 't_d7916c1d: % constraint(s) armed (% already in place)',
                 n, 4 - n;
END
$arm$;
