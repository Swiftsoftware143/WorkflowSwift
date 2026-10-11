-- 086_gemini_and_sendgrid_catalogue_rows.sql
-- WorkflowSwift: two providers the app CAN call were missing from `available_providers`, so the
-- customer console invited a key the API then refused. Found by the off-peak programme run
-- 2026-10-11 (cron `default`), measured live.
--
-- WHAT WAS WRONG (measured on the LIVE app, not inherited):
--   The tenant console's Provider Keys view (www-app/index.html) offers TWELVE slots including
--   `gemini` ("Google Gemini") and `sendgrid` ("SendGrid"), and the user guide (www-app/guide.html)
--   tells a customer their AI step can run on "OpenAI, Anthropic, DeepSeek or Gemini". Both of those
--   providers ARE callable here:
--     * `gemini` is in `crate::ai_llm::AI_PROVIDERS` (base https://generativelanguage.googleapis.com,
--       default model gemini-2.0-flash) with its own request builder, response parser and unit tests;
--     * `sendgrid` is in `crate::email::EMAIL_PROVIDERS` (smtp, mailgun, sendgrid, sendiio).
--   But NEITHER had a row in `available_providers` (20 rows: no gemini, no sendgrid), and
--   `provider_keys_handler::upsert_provider_key` validates every write against that table — so:
--     POST /provider-keys {"provider":"gemini"}   -> HTTP 400 "'gemini' is not an integration this app offers"
--     POST /provider-keys {"provider":"sendgrid"} -> HTTP 400 "'sendgrid' is not an integration this app offers"
--   A customer could pick the provider, paste a real key, and the save was refused by a message that
--   contradicts the console and the guide. Worse for gemini: an AI Action step whose provider is
--   `gemini` is ACCEPTED at creation (`workflow_handler::assert_ai_provider_ok` reads AI_PROVIDERS),
--   so the tenant could build a step that could never obtain a credential.
--
-- WHY THIS IS THE RIGHT DIRECTION (and not "remove the two slots"):
--   The module doc on AI_PROVIDERS states the intent explicitly — "ONE list: the engine's arm, the
--   resolve endpoint and the console's Provider Keys panel all read it, so the app cannot serve a
--   provider it cannot call". Here the engine CAN call both; only the catalogue lagged. 027a seeded
--   available_providers before gemini existed in the engine (gemini arrived with t_03e4d3d9). So the
--   fix is to seat the two rows, exactly as their siblings are seated — same shape as openai /
--   anthropic / deepseek (AI: requires_base_url = true, requires_metadata ["api_key","model"],
--   icon 'zap', no category) and mailgun / sendiio (email: category 'email', icon 'mail').
--
-- ADDITIVE + IDEMPOTENT: two INSERT ... ON CONFLICT (key) DO NOTHING rows; no existing row is
-- touched, nothing is renamed, and re-running it is a no-op. `provider_keys` holds 0 rows in total,
-- so no tenant is affected. No BEGIN/COMMIT — the runner wraps each file.
--
-- The three remaining dead slots in that same dropdown (`mailchimp` — no transport in
-- EMAIL_PROVIDERS and nothing reads it; `coreswift_crm` — the catalogue key is `coreswift`;
-- `webhook` — a per-step Notify CHANNEL whose URL lives in the step config, not a stored provider
-- key) are corrected on the CONSOLE side, not here: none of them is a provider this app can call,
-- so seating a row for them would offer something that still cannot work.

INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon, category, sort_order, is_active) VALUES
    ('gemini',   'Google Gemini API', 'Google Gemini models for AI conversations and analysis',        true, '["api_key", "model"]',              'zap',  NULL,    0, true),
    ('sendgrid', 'SendGrid API',      'Transactional email delivery service (SendGrid v3 mail/send)', true, '["api_key", "from_email"]',         'mail', 'email', 0, true)
ON CONFLICT (key) DO NOTHING;
