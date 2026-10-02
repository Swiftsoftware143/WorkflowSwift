-- 070: arm the LAST `aid` foreign key — email_templates (kanban t_b5e3baa4).
--
-- WHY (measured, live, 2026-10-02 — /opt/swift/audits/t_b5e3baa4/)
--   068 armed `aid -> accounts(id) ON DELETE CASCADE` on the four tables the orphan class was
--   measured on, and left email_templates out because BOTH halves of the cure were blocked by
--   measurement (`ALTER TABLE ... ADD CONSTRAINT` raised 23503 on the nil-UUID sentinel rows;
--   normalising the sentinel to NULL raised 23505 on idx_email_templates_unique, because the live
--   instance carries TWO `welcome` is_default = true rows). This file clears both blockers and
--   arms the constraint, so no account-owned template row can be orphaned again by an account
--   delete (bin/ws-residue-cleanup.sh, the fleet probe sweep) — the class 068 closed.
--
--   The duplicate, resolved by measurement rather than by preference:
--     404dbfc7-c786-4c2a-bf2c-7bd988efafec  "Default Welcome Email"  aid NULL         2026-08-08
--     00000000-0000-0000-0000-000000000001  "Welcome Email"          aid nil sentinel 2026-08-11
--   * src/email.rs:99 resolves the keeper with
--       `WHERE template_type = $1 AND (is_default = true OR is_default IS NULL)
--        ORDER BY is_default DESC, created_at DESC LIMIT 1`
--     i.e. the NEWEST default row — 000...001 on the live instance. Choosing any other keeper
--     would CHANGE the served welcome mail; keeping 000...001 changes nothing on the wire.
--   * 404dbfc7 is independently un-renderable: src/email.rs:57 `render_template` substitutes only
--     `{{key}}` (`format!("{{{{{}}}}}", key)`), and 404dbfc7 carries SIX single-brace placeholders
--     ({app_name}, {name}, {email}, {password}, {login_url}) and ZERO double-brace ones — it would
--     send the braces literally. 000...001 carries 4 double-brace and 0 single-brace.
--   * migrations/066 already classifies 404dbfc7 as an OPERATOR-OWNED row ("Default Welcome Email",
--     created before the shipped 012 rows, measurably shadowed). So it is DEMOTED, never deleted:
--     deleting operator-owned data is not a migration's call, and one UPDATE reverses this. The row
--     stays in the admin list with is_default = false and never changes a served email.
--
-- WHAT THIS DOES — three steps, in this order (step 1 must precede step 2: the 23505 is the
-- `(welcome, COALESCE(aid, nil), is_default = true)` collision, which exists in COALESCE space
-- already, before any aid is rewritten).
--   1. demote, per (template_type, COALESCE(aid, nil-uuid)) group, every default row except the one
--      the send path resolves (newest created_at) — so the FK can be armed without a unique-index
--      collision and the resolved welcome/team_invite/password_reset mail is byte-identical;
--   2. normalise the nil-UUID system sentinel to NULL on email_templates.aid. The schema already
--      equates the two: the partial unique index is
--      `(template_type, COALESCE(aid, nil-uuid), is_default) WHERE aid IS NULL AND is_default`;
--      and the app's own system scope is documented as "owned by no account";
--   3. arm email_templates_aid_fkey — the same name, target and delete action as the four in 068.
--
--   After step 2 the three system templates (welcome / team_invite / password_reset) are `aid IS
--   NULL`: owned by no account, therefore immune to `ON DELETE CASCADE` from any account delete.
--   The app must WRITE NULL for that scope from now on — the two handlers that used to bind
--   `Uuid::nil()` bind `None::<Uuid>` in the same commit (src/handlers/email_templates_handler.rs,
--   src/handlers/admin_settings_handler.rs); a nil-UUID bind after this file raises 23503.
--
-- IDEMPOTENT BY CONSTRUCTION
--   Applied a second time: step 1 finds no group with a second default row (0 demoted), step 2
--   finds no nil-UUID row (0 normalised), step 3 finds the constraint present. It is safe to leave
--   for the next boot, and a FRESH install (012 seeds the three rows, 038's welcome insert is
--   skipped by its own `WHERE NOT EXISTS ... template_type = 'welcome'` guard, so no duplicate ever
--   exists) applies it to the same end state.

-- ── 1. resolve duplicate defaults (demote the shadowed ones; the send path keeps the newest) ───────
DO $dedup$
DECLARE n bigint := 0;
BEGIN
    WITH ranked AS (
        SELECT id,
               row_number() OVER (
                   PARTITION BY template_type, COALESCE(aid, '00000000-0000-0000-0000-000000000000'::uuid)
                   ORDER BY created_at DESC, id DESC
               ) AS rn
          FROM email_templates
         WHERE is_default = true
    ), losers AS (SELECT id FROM ranked WHERE rn > 1)
    UPDATE email_templates e
       SET is_default = false,
           updated_at = now()
      FROM losers l
     WHERE e.id = l.id;
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 't_b5e3baa4: % duplicate default email_templates row(s) demoted (the send path keeps the newest)', n;
END
$dedup$;

-- ── 2. normalise the nil-UUID system sentinel to NULL so the column can carry a real FK ───────────
DO $norm$
DECLARE n bigint := 0;
BEGIN
    UPDATE email_templates
       SET aid = NULL,
           updated_at = now()
     WHERE aid = '00000000-0000-0000-0000-000000000000'::uuid;
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 't_b5e3baa4: % system-scope email_templates row(s) normalised nil -> NULL', n;
END
$norm$;

-- ── 3. arm the FK (same shape as the four armed in 068) ───────────────────────────────────────────
DO $arm$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conname = 'email_templates_aid_fkey'
                  AND conrelid = 'public.email_templates'::regclass) THEN
        RAISE NOTICE 't_b5e3baa4: email_templates_aid_fkey already in place';
    ELSE
        ALTER TABLE public.email_templates
            ADD CONSTRAINT email_templates_aid_fkey
            FOREIGN KEY (aid) REFERENCES public.accounts(id) ON DELETE CASCADE;
        RAISE NOTICE 't_b5e3baa4: armed email_templates.aid -> accounts(id) ON DELETE CASCADE';
    END IF;
END
$arm$;
