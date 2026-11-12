-- SEAL THE HEXOMATIC BYOK KEY AT REST (kanban t_08ac18ca).
--
-- `accounts.hexomatic_key` is a customer-supplied third-party credential: the tenant types its
-- Hexomatic API key into the Prospecting panel and the app replays it. Both write doors
-- (`PUT /accounts/hexomatic-key` and `PUT /accounts`'s `hexomatic_key` field) used to bind the RAW
-- request value, so a database dump, a leaked backup or a read-only SQL grant handed over a usable
-- credential. `provider_keys.api_key` has been sealed since migration 048 with the same envelope
-- (`enc:v1:` + base64 of pgp_sym_encrypt under `PROVIDER_KEY_ENC_SECRET`), and this column now
-- follows it exactly.
--
-- Measured 2026-11-12 before this migration: 12 accounts, 0 non-null hexomatic_key, so there is
-- nothing to backfill. NOT VALID keeps a restored pre-fix dump from aborting a boot, and the DO
-- block VALIDATEs immediately when (as now) nothing plaintext remains, so the guard is fully
-- enforced for the first customer write.
--
-- The constraint is added here and never re-added by the app: this app's runner records every file
-- in `_migrations` and skips it afterwards. A restore that loses the object is repaired by dropping
-- the `_migrations` row and re-running the file.

ALTER TABLE accounts DROP CONSTRAINT IF EXISTS accounts_hexomatic_key_sealed;

ALTER TABLE accounts ADD CONSTRAINT accounts_hexomatic_key_sealed
  CHECK (hexomatic_key IS NULL OR hexomatic_key = '' OR hexomatic_key LIKE 'enc:v1:%') NOT VALID;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM accounts
     WHERE hexomatic_key IS NOT NULL
       AND hexomatic_key <> ''
       AND hexomatic_key NOT LIKE 'enc:v1:%'
  ) THEN
    ALTER TABLE accounts VALIDATE CONSTRAINT accounts_hexomatic_key_sealed;
  END IF;
END $$;
