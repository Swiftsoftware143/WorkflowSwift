-- 098 — Google Places becomes CONNECTABLE, because Prospecting now uses the account's own key.
--
-- Prospecting (src/handlers/prospecting_handler.rs) used to fabricate businesses, contact emails and
-- phone numbers. It now reads the account's own Google Places credential from `provider_keys`
-- (`enc:v1:` ciphertext, decrypted through crate::security::provider_key_crypto) and returns exactly
-- what the provider said — or refuses with a sentence naming the missing key.
--
-- The catalogue row already existed as this app's data provider, but `coming_soon = true`, and that
-- flag is what makes POST /api/v1/provider-keys refuse to store a key for it ("… is Coming Soon and
-- cannot be connected yet"). Without this UPDATE the honest refusal could never be resolved by the
-- customer: there was no screen state in which a key could be connected at all.
--
-- Scope: `coming_soon` is read in exactly ONE place in this app (provider_keys_handler, both the
-- upsert gate and the catalogue read), so this arms the Provider Keys entry and nothing else — no
-- integration destination, step dispatcher or webhook path selects on it.
--
-- Idempotent: a second boot re-applies the same single row to the same value.
UPDATE available_providers
   SET coming_soon = false
 WHERE key = 'google_places';
