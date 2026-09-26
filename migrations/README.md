# WorkflowSwift migrations — ownership of seed rows (what ships, what the operator owns)

Runner: `src/db.rs` reads `./migrations/*.sql`, **sorts by filename (byte order)** and applies every
file whose FILENAME is not already in `_migrations`, one `sqlx::raw_sql` batch per file. An
unrecorded failure is fatal (`Refusing to serve: N migration file(s)`).

Two consequences that decide every question below:

* an **edited** file only reaches fresh installs — production already has the filename recorded and
  the runner skips it (the ledger is filename-keyed, no checksum);
* a **new** file reaches production at the next boot, so it must be idempotent and it must be
  intended for production. Migrations are baked into the image (`COPY migrations /app/migrations`),
  so a new file reaches live only through a rebuild + recreate — never a restart.

## Row sets the PRODUCT ships (seeded by migrations, identical on a fresh install and in production)

`available_providers` (20), `integration_provider_presets` (8), `plan_tiers` (4),
`plan_capabilities` (24), `industries` (6), `template_categories` (6), `admin_settings`
(7 keys: retention, signup, branding, billing, security, limits, email), `integration_destinations`
(18) and `feature_limits` (4) — the last two restored in production by
`066_seed_integration_destinations_and_feature_limits.sql` (see that file's header; they were lost
by the 2026-08-09 install, which ran the old error-swallowing runner).

A catalogue the shipped code READS belongs in a guarded migration and nowhere else. The test used
here: a row set is product-owned when the value is authored in this repository **and** a mounted
route would answer differently without it.

## Row sets the OPERATOR owns — do NOT seed these, document them instead

Measured (kanban t_82d61045, audit `/opt/swift/audits/t_82d61045/`). Each is a real fresh-vs-live
difference that a migration must **not** close, with the reason and what a fresh install does
instead.

| row(s) | where it lives | why it is NOT seeded | what a fresh install serves instead |
|---|---|---|---|
| `admin_settings.workflowswift_site` (6 189 bytes, `updated_by` = the owner, 2026-08-11) | production only | written by the operator through `PUT /api/v1/admin/site` (SEO / tracking / homepage copy — customer-specific content, not product content) | `GET /api/v1/admin/site` merges `default_site_settings()` with the row, so a fresh install answers the defaults with HTTP 200. The generated HTML is produced by `regenerate_html()` on the operator's first save |
| `email_templates` "Default Welcome Email" (aid NULL, `welcome`, created 2026-08-08 — before the shipped `012` rows) | production only, out of band | not created by any file in this repository; it predates the shipped templates. It is also **measurably inert**: the lookup (`src/email.rs`, `ORDER BY is_default DESC, created_at DESC LIMIT 1`) resolves the newer `Welcome Email` row from `012` on live and on a fresh install alike — verified, both return the same row | `012_seed_email_templates.sql` inserts Welcome / Team Invite / Password Reset |
| `users` row `admin@swiftsoftware.com`, `role='admin'` (`040_seed_admin_user.sql`) | **fresh installs only** — production has no `role='admin'` row at all | production deliberately stays un-normalised: its admin identity is the owner's own `super_admin` row. Decision and evidence in t_2255fdea (commit `3a95468`), which also removed `011_role_cleanup.sql`'s UPDATEs. No `role='admin'` row is to be created in production — least of all one whose bcrypt hash ships in the repository | one user row (`admin@swiftsoftware.com`), which is what a first login needs |
| `admin_settings.email` — row present on both, **value** differs (live 475 bytes, rewritten by the owner 2026-09-25; fresh 225 bytes from `037`) | both | the row is product-seeded; the live value is the operator's SMTP/provider configuration entered in Admin → Settings → Email (`037` seeds a default, the UI overwrites it) | the `037` default |
| `user_api_keys` (201 live), accounts, contacts, workflow instances, `dashboard_widgets`, … | production carries real customer/operator data | runtime data, not seed rows | empty |

Rule of thumb: **authored in this repo + read by a shipped route ⇒ seed it; typed in by the owner
in the UI, or data the app generates ⇒ document it.** Pricing, plan entitlements, provider
credentials, site copy and customer rows are the operator's; if a future change wants one of them
normalised, it is a product decision to be made on a card, not a side effect of a migration.

## Post-install (fresh install) — what an operator must enter, in order

1. Log in as `admin@swiftsoftware.com` with the temporary password from `040_seed_admin_user.sql`
   and change it immediately (the hash is in the repository).
2. Admin → Settings → **Email**: enter the provider + credentials (DB `admin_settings.email`; the
   app has no `EMAIL_*` env fallback — a fresh install sends no mail until this is set).
3. Admin → Settings → **Site**: fill in SEO / tracking / homepage and save; this writes
   `admin_settings.workflowswift_site` and regenerates the served HTML.
4. Admin → **Plans**: review the four tiers and their limits. `feature_limits` and
   `plan_tiers.features` seed the same values; anything changed here should be changed in
   `plan_tiers.features` (the resolution order in `src/features.rs` is features JSONB → legacy
   column → `feature_limits`).
5. Admin → **Email templates**: the three shipped defaults are the fallback; edit rather than add
   duplicates (the lookup picks `is_default DESC, created_at DESC`).
6. Add provider keys (Admin → Integrations / `provider_keys`, encrypted at rest via
   `PROVIDER_KEY_ENC_SECRET`).
