# Architecture & Naming — Locked (2026-07-11 13:58)

**David settled this. No more debates.**

## Naming Convention (Final)

| DB / Code | UI / Docs | Meaning |
|---|---|---|
| `user` | User | The SaaS customer who signs up. Each gets isolated workspace (tenant_id in DB). |
| `team_member` | Team Member | Additional people invited under a user. |
| (DB only: `tenant_id`) | — | Column name for data isolation. Never shown to users. Never called "tenant" in docs or UI. |

**Rule:** When talking to David (the operator), refer to the SaaS customers as "users." When talking to customers in docs/UI, call them "Users." No "tenant" anywhere visible.

## Architecture (David's Words)

- **Engine:** OpenClaw + n8n — reasoning, orchestration, execution
- **Interface:** WorkflowSwift SaaS — visual layer, templates, dashboards, workflow builder
- **Extension:** Browser extension for data injection

The SaaS is a visual wrapper that makes OpenClaw's capabilities usable by non-technical people. OpenClaw does the thinking/acting; WorkflowSwift presents it with context.

## Pricing Model

- **BYOK user** — brings own API key → no credit consumption, just platform fee
- **No-key user** — uses WorkflowSwift pooled keys → credits consumed per execution

## Build Priority

1. SendGrid send_template_email destination + SMTP provider seed
2. Login/registration UI overhaul (separate admin login from user signup)
3. AI Action step type → routes LLM calls through OpenClaw
