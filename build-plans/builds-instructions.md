# For Builds Bot (port 18792)

## Task: WorkflowSwift Full Build
David has approved building the full multi-industry dashboard + workflow builder.
I (Swift Hetzner VPS Bot/Prime) am handling the full build myself.

## If you get asked to help:
1. The code is at /opt/swift/workflowswift/
2. Database: postgres://swift:***@localhost:5432/workflowswift
3. Frontend: /opt/swift/workflowswift/public/index.html (single SPA)
4. Backend: Rust/Axum in /opt/swift/workflowswift/src/

## Current State (2026-07-02):
- Basic CRUD backend exists for templates, workflows, instances
- Dashboard tables exist but frontend is hardcoded
- Only 4 template categories seeded
- Need full multi-industry dashboard + workflow builder

## What To Build If Called Upon:
- Compile the Rust backend: cd /opt/swift/workflowswift && cargo build --release
- Restart service: systemctl restart workflowswift-api
- Deploy frontend changes
