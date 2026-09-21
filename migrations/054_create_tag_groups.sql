-- 054_create_tag_groups.sql
-- Real per-account tag groups for WorkflowSwift.
--
-- Replaces the auto-generated tag_groups stub (kanban t_01fa9bbc): the handler read a
-- 'tag_groups' relation that no migration ever created, was not auth/tenant-scoped, and
-- swallowed the missing-relation error with unwrap_or_default(), so GET answered an empty
-- 200 list while POST/PUT/DELETE answered 500 'relation "tag_groups" does not exist'.
-- The admin shell ships the screen (Tags and Labels -> Tag Groups), so the table is the
-- honest fix. Mirrors the 044_create_tickets.sql precedent. The admin form posts
-- name + description, so both live here.
--
-- Idempotent: safe to re-run (the migration runner re-runs any file it could not record).
CREATE TABLE IF NOT EXISTS tag_groups (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    aid         UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_tag_groups_aid ON tag_groups(aid);
