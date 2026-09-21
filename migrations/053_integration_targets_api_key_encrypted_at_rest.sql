-- 053_integration_targets_api_key_encrypted_at_rest.sql
--
-- integration_targets.api_key holds a TENANT-SUPPLIED third-party credential: the bearer token a
-- customer's integration target expects on the outbound dispatch. It is the sibling defect to
-- 048_provider_keys_encrypted_at_rest.sql — a different table in this same app, same family, and
-- it was the one credential column in WorkflowSwift that never got the guard.
--
-- Before this change the only writer, create_integration_target, bound the raw request value
-- straight into the column, so any target a customer saved with a key sat in the clear at rest and
-- would fall out of a database dump, a leaked backup or a read-only SQL grant. Unlike ADASwift's
-- twin of this column, WorkflowSwift DOES read it for use (instance_handler::advance_instance puts
-- it on the wire as an Authorization / x-api-key header), so the ciphertext is decrypted at that
-- one read-for-use site and never forwarded in its stored form.
--
-- The write now goes through src/security/provider_key_crypto.rs (encrypt_for_storage), the same
-- choke point provider_keys uses: AES-256 via pgcrypto, master key held ONLY in the process
-- environment (PROVIDER_KEY_ENC_SECRET), stored as 'enc:v1:' plus single-line base64 ciphertext. A
-- missing master key makes the write FAIL CLOSED rather than storing the plaintext.
--
-- This constraint is the regression guard: a future writer that forgets to encrypt FAILS CLOSED at
-- the database instead of silently persisting a plaintext credential. NULL (key not supplied) and
-- the empty string (key cleared) both stay allowed so "no credential" is still representable.
--
-- WHO RUNS THIS FILE (WorkflowSwift has the apply path the ADASwift twin lacked): src/db.rs
-- run_migrations applies every ./migrations/*.sql file not yet recorded in _migrations, at boot,
-- each file as ONE simple-query batch (sqlx::raw_sql, no splitting on ';'), and a file that fails
-- is NOT recorded and aborts the boot. So a fresh or restored database cannot come up without this
-- guard, and a re-run is harmless because every statement below is idempotent. Degradation of the
-- validated flag is watched by /opt/swift/bin/guard-constraint-validity-sweep.sh (cron */30).
--
-- A validation failure is caught as check_violation (WARNING, constraint left NOT VALID, exit 0):
-- never a failed deploy, never a silently dropped guard. Canonical idiom, verbatim from
-- /opt/swift/fleet/templates/guard-constraint-not-valid.sql (kanban t_c9b09cc1), same shape as
-- ADASwift migrations/000013 and missedcallrespondr's integration_targets guard.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE integration_targets DROP CONSTRAINT IF EXISTS integration_targets_api_key_encrypted;

ALTER TABLE integration_targets ADD CONSTRAINT integration_targets_api_key_encrypted CHECK (api_key IS NULL OR api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'integration_targets_api_key_encrypted'
          AND conrelid = 'integration_targets'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE integration_targets VALIDATE CONSTRAINT integration_targets_api_key_encrypted;
            RAISE NOTICE 'integration_targets.integration_targets_api_key_encrypted validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'integration_targets.integration_targets_api_key_encrypted still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file); new writes are still rejected';
        END;
    END IF;
END
$guard$;
