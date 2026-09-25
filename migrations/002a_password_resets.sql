-- 002a: password_resets — RENAMED from `000004_password_resets.sql` (card t_4ebd6f98).
--
-- WHY THE RENAME
--   `src/db.rs` orders files by FILE NAME (`entries.sort_by_key(|e| e.file_name())`, byte order),
--   and "000004_password_resets.sql" sorts BEFORE "001_create_tenants.sql" / "002_create_users.sql"
--   (position 2: '0' < '1'), so the file ran before the table it references and a from-zero build
--   died with `ERROR: relation "users" does not exist` — the first of the two blockers this card
--   measured. The name is the bug: nothing here depends on running first.
--
--   `002a` sorts after `002_create_users.sql` ('_' 0x5F < 'a' 0x61, and '2' < '3') and before
--   `003_create_clients.sql`, so the file now runs after the users table it FKs.
--
--   The old name stays in the production `_migrations` ledger — the runner keys on the FILENAME,
--   so the ledger row for `000004_password_resets.sql` is simply never matched again, and this
--   file is applied once on the next boot. It is a no-op there: `CREATE TABLE IF NOT EXISTS` on a
--   table production has had since 2026-08-09, so the rename changes nothing in production and
--   only repairs the from-zero path.
CREATE TABLE IF NOT EXISTS password_resets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    token TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
