-- 099 — Competitor Watch keeps a SNAPSHOT fingerprint, so "no changes detected" can be true.
--
-- check_competitor (src/handlers/competitor_watch_handler.rs) used to answer
-- `changes_detected: false` with the sentence "Checked competitor '<name>'. No significant changes
-- detected." WITHOUT fetching anything — a permanent, confident false negative, and `sample_data:true`
-- was the only hint.
--
-- The check is now real: fetch the competitor's own website through the app's existing SSRF gate
-- (security::webhook_security::gate_step_destination), hash the bytes, and compare with what the last
-- check saw. That comparison needs somewhere to live, which is what these three columns are:
--
--   snapshot_hash  — sha256 (hex) of the last body successfully fetched
--   snapshot_bytes — its size, so a summary can say how much moved
--   snapshot_at    — when that body was seen (the baseline's age is part of an honest answer)
--
-- Null means "never checked": the first check records a baseline and says so instead of claiming a
-- comparison it could not make. Idempotent (IF NOT EXISTS), so the boot-time runner can re-apply it.
ALTER TABLE competitors ADD COLUMN IF NOT EXISTS snapshot_hash  TEXT;
ALTER TABLE competitors ADD COLUMN IF NOT EXISTS snapshot_bytes BIGINT;
ALTER TABLE competitors ADD COLUMN IF NOT EXISTS snapshot_at    TIMESTAMPTZ;
