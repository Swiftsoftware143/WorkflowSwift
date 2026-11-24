-- 134_account_erasure.sql — a customer can DELETE their own account and data (2026-11-24,
-- kanban t_9999fc8e, fleet standard docs/account-closure-erasure-standard-2026-11-24.md §3-§7).
--
-- `close_account` (migration 113) records a STATE: the rows stay and support can reopen. Erasure
-- is the other half and is irreversible: `POST /api/v1/auth/erase-data` destroys the account through
-- the ONE SQL writer `erase_tenant(uuid, uuid, text)`, which deletes child-first across every table
-- that names the account, then the account row itself (whose FKs CASCADE anything a pass missed).
--
-- Two things must SURVIVE that cascade, and because every FK to `accounts` is ON DELETE CASCADE a
-- surviving row cannot carry an account FK at all. Hence two tables with a plain uuid column and NO
-- foreign key, deliberate and load-bearing:
--
--   * `erasure_billing_retention` — the billing DOCUMENTS tax law requires us to keep (the plan the
--     customer was on), copied row-for-row before the delete as jsonb. The copy is detached from the
--     account NAME on purpose: it carries the erased account's uuid and the stored plan facts, never
--     the company name, slug or an email address.
--   * `account_erasures` — exactly ONE audit row per erasure: who asked (the signed-in user), when,
--     and the counts that were destroyed. The account's own `audit_logs` rows go with the cascade;
--     this row is the record that the erasure itself happened, and it never stores the password or
--     the session token (§3.5).
--
-- Idempotent by construction (IF NOT EXISTS), so a re-run is a no-op.

CREATE TABLE IF NOT EXISTS erasure_billing_retention (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The erased account's id. NO foreign key: the account row is gone by design.
    erased_tenant_id uuid NOT NULL,
    -- 'account_plans' (the plan the customer was on).
    source           text NOT NULL,
    -- The whole source row as jsonb: plan id, status, dates.
    document         jsonb NOT NULL,
    retained_at      timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS erasure_billing_retention_tenant_idx
    ON erasure_billing_retention (erased_tenant_id);

COMMENT ON TABLE erasure_billing_retention IS
    'Billing documents kept after a customer erases their account (tax retention). No FK to accounts: the account no longer exists.';

CREATE TABLE IF NOT EXISTS account_erasures (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The erased account. NO foreign key, for the same reason as above.
    erased_tenant_id uuid NOT NULL,
    -- Who asked. The user row CASCADEs away with the account, so the id is kept here as a value.
    actor_user_id    uuid NOT NULL,
    actor_email      text NOT NULL,
    action           text NOT NULL DEFAULT 'erase_data',
    -- What was destroyed, and the billing documents retained.
    records          jsonb NOT NULL DEFAULT '{}'::jsonb,
    erased_at        timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS account_erasures_erased_at_idx
    ON account_erasures (erased_at DESC);

COMMENT ON TABLE account_erasures IS
    'One row per successful self-service data erasure (who, when, what was destroyed). Never stores a password or a token.';

-- retain the tax documents: invoices stop pointing at an account that will not exist
ALTER TABLE invoices ALTER COLUMN aid DROP NOT NULL;

-- ONE function, identical body across the fleet, only the three constants at the top differ:
--   v_root_table = 'accounts'
--   v_root_cols  = the columns children use to name their account (a MIX here: aid, tenant_id,
--                  account_id)
--   v_plan_table = 'account_plans'
--
-- The order is load-bearing and must not drift: billing documents FIRST (detach, never delete),
-- then a child-first delete over every table that names the account, then the account row itself,
-- then a census that RAISEs if anything still names it, then ONE audit row.
CREATE OR REPLACE FUNCTION erase_tenant(p_root uuid, p_actor uuid, p_actor_email text)
RETURNS jsonb LANGUAGE plpgsql AS $$
DECLARE
  v_root_table  constant text   := 'accounts';
  v_root_cols   constant text[] := ARRAY['aid','tenant_id','account_id'];
  v_plan_table  constant text   := 'account_plans';
  r             record;
  v_pass        int := 0;
  v_rows        bigint := 0;
  v_tables      bigint := 0;
  v_billing     bigint := 0;
  v_invoices    bigint := 0;
  v_left        bigint;
  v_audit       uuid;
  v_n           bigint;
  v_pass_rows   bigint;
BEGIN
  -- 0. billing documents are KEPT. Invoices are detached from the account, never deleted.
  FOR r IN SELECT table_name, column_name FROM information_schema.columns
            WHERE table_schema='public' AND table_name='invoices' AND column_name = ANY(v_root_cols) LOOP
    EXECUTE format('UPDATE %I SET %I = NULL WHERE %I = $1', r.table_name, r.column_name, r.column_name)
      USING p_root;
    GET DIAGNOSTICS v_invoices = ROW_COUNT;
    EXIT;
  END LOOP;

  -- 0b. the plan the customer was on is a billing document too: copy it, detached, then it cascades.
  FOR r IN SELECT column_name FROM information_schema.columns
            WHERE table_schema='public' AND table_name=v_plan_table AND column_name = ANY(v_root_cols) LOOP
    EXECUTE format('INSERT INTO erasure_billing_retention (erased_tenant_id, source, document)
                    SELECT $1, %L, to_jsonb(t) FROM %I t WHERE t.%I = $1', v_plan_table, v_plan_table, r.column_name)
      USING p_root;
    GET DIAGNOSTICS v_billing = ROW_COUNT;
    EXIT;
  END LOOP;

  -- 1. child-first delete over every table that names this account. Retry (up to 12 passes)
  --    tolerating foreign_key_violation so a NO ACTION / RESTRICT sibling is retried after the
  --    row blocking it is gone. Any other error RAISEs: success is never reported on a partial purge.
  --    BASE TABLE only: a VIEW (e.g. `credit_balance`) also exposes a root column in
  --    information_schema.columns but cannot be DELETEd, so views are excluded from the purge.
  WHILE v_pass < 12 LOOP
    v_pass := v_pass + 1;
    v_pass_rows := 0;
    FOR r IN SELECT c.table_name, c.column_name
              FROM information_schema.columns c
              JOIN information_schema.tables t
                ON t.table_name = c.table_name AND t.table_schema = c.table_schema
              WHERE c.table_schema='public' AND t.table_type='BASE TABLE'
                AND c.column_name = ANY(v_root_cols)
                AND c.table_name <> 'invoices'
                AND c.table_name <> v_root_table LOOP
      BEGIN
        EXECUTE format('DELETE FROM %I WHERE %I = $1', r.table_name, r.column_name) USING p_root;
        GET DIAGNOSTICS v_n = ROW_COUNT;
        IF v_n > 0 THEN
          v_tables := v_tables + 1;
          v_rows := v_rows + v_n;
          v_pass_rows := v_pass_rows + v_n;
        END IF;
      EXCEPTION WHEN foreign_key_violation THEN
        NULL;  -- a sibling still points here; the next pass retries after it is deleted
      END;
    END LOOP;
    -- A full pass deleted nothing new: either the tree is empty or a blocker the census below will
    -- catch. Stop rather than spin the remaining budget on an unchanged graph.
    EXIT WHEN v_pass_rows = 0;
  END LOOP;

  -- 2. the account itself (its own FKs CASCADE)
  EXECUTE format('DELETE FROM %I WHERE id = $1', v_root_table) USING p_root;

  -- 3. census: nothing may still name the account except the retained invoices (NULL now). BASE
  --    TABLE only, for the same reason as the delete loop (a view cannot be purged and must not be
  --    counted as leftover).
  SELECT COALESCE(sum(c), 0) INTO v_left FROM (
    SELECT (xpath('/row/c/text()', query_to_xml(
       format('SELECT count(*) AS c FROM %I WHERE %I = %L', c.table_name, c.column_name, p_root),
       false, true, '')))[1]::text::bigint AS c
    FROM information_schema.columns c
    JOIN information_schema.tables t
      ON t.table_name = c.table_name AND t.table_schema = c.table_schema
    WHERE c.table_schema='public' AND t.table_type='BASE TABLE'
      AND c.column_name = ANY(v_root_cols)
      AND c.table_name NOT IN ('invoices', v_root_table)
  ) s;
  IF v_left > 0 THEN
    RAISE EXCEPTION 'erasure incomplete: % rows still reference account %', v_left, p_root;
  END IF;

  -- 4. ONE audit row that survives the purge (no FK, no password, no token).
  INSERT INTO account_erasures (erased_tenant_id, actor_user_id, actor_email, action, records)
  VALUES (p_root, p_actor, p_actor_email, 'erase_data',
          jsonb_build_object('rows_deleted', v_rows, 'tables_touched', v_tables,
                             'billing_documents_retained', v_billing,
                             'invoices_retained', v_invoices))
  RETURNING id INTO v_audit;

  RETURN jsonb_build_object(
    'status', 'account_erased',
    'erased_tenant_id', p_root,
    'erased_at', now(),
    'audit_id', v_audit,
    'records', jsonb_build_object('rows_deleted', v_rows, 'tables_touched', v_tables,
                                  'billing_documents_retained', v_billing,
                                  'invoices_retained', v_invoices));
END $$;
