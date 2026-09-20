-- 045_integration_center_coreswift.sql
--
-- Integration Center standard (2026-09-20): the canonical CoreSwift spoke.
--
-- Idempotent and safe to re-run. NOTE: WorkflowSwift's migration runner splits each file
-- on the statement separator and swallows failures, so this file keeps every statement
-- independently valid and keeps comments free of the separator character.
--
-- 1. Make the `coreswift` catalogue row match the fleet standard exactly
--    (key='coreswift', name='CoreSwift CRM', description='Push leads into CoreSwift CRM',
--     requires_base_url=false, requires_metadata='[]'), creating it if this app lacks it.
-- 2. Seed `integration_provider_presets.base_url` for key='coreswift' — step 2 of the
--    documented base-URL resolution order (provider_keys override -> preset -> constant).
-- 3. Add the `n8n` row: WorkflowSwift's own domain provider (key + instance base URL).

INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon, is_active)
VALUES ('coreswift', 'CoreSwift CRM', 'Push leads into CoreSwift CRM', false, '[]'::jsonb, '🔗', true)
ON CONFLICT (key) DO NOTHING;

UPDATE available_providers
SET name = 'CoreSwift CRM',
    description = 'Push leads into CoreSwift CRM',
    requires_base_url = false,
    requires_metadata = '[]'::jsonb,
    is_active = true
WHERE key = 'coreswift';

INSERT INTO integration_provider_presets (key, name, base_url, docs_url, sort_order, is_active)
VALUES ('coreswift', 'CoreSwift CRM', 'http://localhost:8084', 'https://coreswiftcrm.com/docs', -10, true)
ON CONFLICT (key) DO UPDATE
SET name = EXCLUDED.name,
    base_url = EXCLUDED.base_url,
    docs_url = EXCLUDED.docs_url,
    is_active = true;

INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon, is_active)
VALUES ('n8n', 'n8n Automation', 'Run your own n8n workflows from WorkflowSwift', true, '["api_key"]'::jsonb, '🎛️', true)
ON CONFLICT (key) DO NOTHING;

INSERT INTO integration_provider_presets (key, name, base_url, docs_url, sort_order, is_active)
VALUES ('n8n', 'n8n Automation', 'http://localhost:5678', 'https://docs.n8n.io', 0, true)
ON CONFLICT (key) DO NOTHING;
