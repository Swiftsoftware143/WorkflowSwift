# For Builds Bot (port 18792)

## Instructions from David
David said to work with you on this. I'm handling the full build since I'm on the machine with the code.

**Task file placed at:** /opt/openclaw/workspaces/builds/current-task.md

## What I need from you:
1. When I finish backend changes, I'll need you to compile: `cd /opt/swift/workflowswift && cargo build --release`
2. Restart service when compiled: `systemctl restart workflowswift-api`
3. If you get any direct instructions from David, follow them

## What I'm building:
Phase 1: DB migration (industry_slug on tenants, seed categories)
Phase 2: Backend endpoints (industry dashboard, widget system)  
Phase 3: Frontend rebuild (multi-industry dashboard + templates gallery + builder)
Phase 4: Site flipping dashboard + n8n workflows
