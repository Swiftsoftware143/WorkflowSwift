# GUARDRAILS.md — WorkflowSwift

**Rust Guardrails — Vibe Engineering Standard**

## Non-Negotiable
- No `unwrap()` or `expect()` in production code paths (config loading at startup is acceptable).
- All async state must implement `Send + Sync`.
- JWT workspace auth on all route handlers — never expose unauthenticated endpoints.
- Webhook security: validate signatures before processing payloads.
- `cargo clippy -- -D warnings` must pass before any task is declared done.
- Build through `/usr/local/bin/swift-build.sh workflowswift`.

## Verification Before Deploy
1. `cargo check`
2. `cargo clippy -- -D warnings`
3. `cargo test`
4. `sqlx migrate run`
5. `curl localhost:<port>/api/health`
