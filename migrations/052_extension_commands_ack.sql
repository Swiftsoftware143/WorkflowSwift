-- 052: acknowledgement support for the Chrome-extension bridge (kanban t_6db12ebe).
--
-- The shipped Swift Market Intel extension posts to POST /api/v1/bridge/commands/ack
-- after it executes a command it polled from GET /bridge/commands. Without somewhere to
-- record the acknowledgement there was nothing for that call to write: rows stayed at
-- 'delivered' forever, no operator surface could tell which commands had actually run,
-- and any workflow polling for completion waited forever.
--
-- READ BEFORE EDITING THIS FILE. The migration runner in src/db.rs splits the whole file
-- on EVERY semicolon character, then runs each fragment while logging statement errors
-- as a non-fatal warning AND still recording the file as "applied" (see run_migrations).
-- A semicolon inside a comment or a string literal therefore silently truncates the
-- migration: the two earlier names of this same file hit exactly that, and
-- acknowledged_at was never created while the file was already marked applied.
-- So: one statement per line, no semicolon character anywhere else in the file, and
-- every statement guarded with IF NOT EXISTS so a re-run over a database that already
-- has the column is a no-op.
ALTER TABLE extension_commands ADD COLUMN IF NOT EXISTS acknowledged_at TIMESTAMPTZ DEFAULT NULL;
ALTER TABLE extension_commands ADD COLUMN IF NOT EXISTS result JSONB DEFAULT NULL;
CREATE INDEX IF NOT EXISTS idx_extension_commands_tenant_ack ON extension_commands(tenant_id, acknowledged_at);
