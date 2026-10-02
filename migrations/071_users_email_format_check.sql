-- 071_users_email_format_check
--
-- WHY (kanban t_09e76b27). `public.users.email` is an account's login identity AND the only address
-- its credentials/welcome mail can ever be delivered to. The column was `varchar(255) NOT NULL`
-- with a UNIQUE(aid, email) and NO `CHECK`, and `POST /api/v1/auth/register` — the public signup —
-- asked only `req.email.is_empty()`, so any string became a real login and a real tenant: an account
-- whose address no mail can ever reach. The same `is_empty`-only guard (or no guard at all) sat on
-- the other three writers of this column (admin_create_account, invite_user, deliver_credentials).
-- Measured across the class by t_4722a331; reference shape proven on missedcallrespondr (t_54b1ffab).
--
-- Every request-path writer of `users.email` now normalises (trim + lowercase) and syntax-checks the
-- address before its first SELECT (src/security/email_addr.rs::normalize, called from
-- auth::handlers::register, handlers::admin_settings_handler::admin_create_account,
-- handlers::user_handler::invite_user and handlers::checkout_handler::deliver_credentials), and
-- login / forgot_password read through lookup_key + `lower(email) = $1`. This constraint is the
-- store-level backstop for the writers nobody has written yet — the same "fix the class, not the
-- call site" posture the fleet applies elsewhere.
--
-- The pattern is deliberately LOOSER than the Rust validator so the database can never refuse a value
-- the application accepted: the application additionally rejects whitespace/control characters, empty
-- and dot-only local/domain parts, and over-long addresses. Everything this regex requires — a non-empty
-- part, one `@`, a non-empty dotted domain — is required by the application too. Plus-aliases
-- (`a+b@x.com`), dotted locals (`a.b@x.com`) and IDN domains (`user@münchen.de`) pass both.
--
-- Idempotent for the boot-time runner (src/db.rs re-executes every registered file on every boot, and
-- a file that throws is an outage): `ADD CONSTRAINT` has no `IF NOT EXISTS`, so it is guarded by a
-- pg_constraint probe. Every live row was checked against the pattern before this constraint was
-- added (6 rows at deploy time: 0 violations), and the guard makes a re-run a no-op.

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'users_email_format_check'
          AND conrelid = 'public.users'::regclass
    ) THEN
        ALTER TABLE public.users
            ADD CONSTRAINT users_email_format_check
            CHECK (email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$');
    END IF;
END
$$;
