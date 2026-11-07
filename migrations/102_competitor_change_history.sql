-- 102_competitor_change_history.sql
--
-- A DETECTED CHANGE MUST SURVIVE THE NEXT CHECK.
--
-- `competitors.snapshot_hash` / `snapshot_bytes` / `snapshot_at` hold only the LATEST page a check
-- fetched, so the second check overwrote the first: the customer was told "this page changed" and
-- that fact was gone after the next run, with nothing anywhere recording WHEN it changed or how much.
-- The check now writes one row here for the baseline (the first successful check) and one for every
-- subsequent difference, so the console can show a change history instead of a single pair of numbers.
--
-- `aid` is the tenant id, the same column `competitors` uses. The foreign key cascades, so removing a
-- competitor takes its history with it and the table cannot accumulate rows for a deleted row.
--
-- Idempotent: this app's runtime runner reads ./migrations at every boot and applies only the files
-- not already recorded in `_migrations`, and the statements below also carry the repeatable form so a
-- rehearsal (`psql -f` inside BEGIN/ROLLBACK) or a re-run can never fail the boot.
CREATE TABLE IF NOT EXISTS competitor_changes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    competitor_id UUID NOT NULL REFERENCES competitors(id) ON DELETE CASCADE,
    aid UUID NOT NULL,
    change_type VARCHAR(50) NOT NULL,
    description TEXT,
    source_url TEXT,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_competitor_changes_comp
    ON competitor_changes(competitor_id, detected_at DESC);

CREATE INDEX IF NOT EXISTS idx_competitor_changes_aid
    ON competitor_changes(aid);
