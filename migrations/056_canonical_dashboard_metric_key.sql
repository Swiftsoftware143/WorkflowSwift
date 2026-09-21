-- 056_canonical_dashboard_metric_key.sql
-- One key space for dashboard series.
--
-- Writers disagreed about the "n8n_" prefix: push_widget_data prefixed only when the caller had
-- not, while push_dashboard_data and internal seed_dashboard_data prefixed unconditionally. For a
-- widget whose dashboard_widgets.config->>'metric_key' was already n8n_-prefixed, the unconditional
-- writers stored n8n_n8n_<x> (which the unconditional read found) while push_widget_data stored
-- n8n_<x> (which nothing read) — so pushes to those widgets were silently lost. Measured
-- 2026-09-21: 7 such rows, all written by one n8n seed run at 2026-07-11 04:21:58Z.
--
-- Collapse every leading repetition to exactly one prefix, so the rows the old read resolved under
-- 'n8n_n8n_<x>' keep resolving under the canonical 'n8n_<x>'. Idempotent: a second run matches 0
-- rows. No collision with existing 'n8n_<x>' rows (checked before writing this migration: 0).
UPDATE dashboard_data
   SET metric_key = 'n8n_' || regexp_replace(metric_key, '^(n8n_)+', '')
 WHERE metric_key LIKE 'n8n_n8n_%';
