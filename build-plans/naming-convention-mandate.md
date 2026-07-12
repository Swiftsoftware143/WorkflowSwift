# Naming Convention — All Bots Must Follow

**Date:** 2026-07-11
**Source:** David (via Builds Bot)
**Status:** 🔒 LOCKED — no further debate

## Rule

1. **User** = the SaaS customer. This is what they are called in the DB, code, UI, and docs.
2. **Team Member** = additional people invited under a user.
3. **Tenant** = internal DB abstraction only. Never used in user-facing code, UI, or documentation.

## Why

David: "They are not the owner of the SaaS. Calling them tenants is confusing."

## What Each Bot Should Do

- **SwiftSoftware (Prime)** — API responses should say `user`, not `tenant`. Frontend labels too.
- **Automation** — n8n templates and workflow outputs should reference `user`, not `tenant`.
- **Monitoring** — alert labels and notifications should say `user`.
- **SwiftImpact** — marketing and docs: no "tenant" anywhere.
- **ZaarHub** — business ops: user-facing materials say "user" or "account."
- **GiraudyCapital** — same applies.

## Exceptions (Internal Only)

- `tenant_id` column in DB stays as-is
- Backend Rust code uses `Claims.tid` internally (this is fine)
- `provider_keys.tenant_id` FK stays — it's the correct isolation mechanism

## Priorities

1. SendGrid send_template + SMTP provider
2. Login/registration UI
3. AI Action step routing through OpenClaw

## Signed

David → Builds Bot → All Bots
