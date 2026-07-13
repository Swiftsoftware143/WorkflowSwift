#!/bin/bash
# Fix auto-generated handler files (struct field rename only)
# These files have: pub tenant_id: Uuid -> pub aid: Uuid
# They also have local variable tenant_id -> aid

cd /opt/swift/workflowswift

for f in \
  src/handlers/affiliates_handler.rs \
  src/handlers/calendar_events_handler.rs \
  src/handlers/call_logs_handler.rs \
  src/handlers/campaigns_handler.rs \
  src/handlers/categories_handler.rs \
  src/handlers/deals_handler.rs \
  src/handlers/email_templates_handler.rs \
  src/handlers/knowledge_base_handler.rs \
  src/handlers/leads_handler.rs \
  src/handlers/reports_handler.rs \
  src/handlers/reviews_handler.rs \
  src/handlers/surfaces_handler.rs \
  src/handlers/tag_groups_handler.rs \
  src/handlers/tickets_handler.rs \
  src/handlers/webhooks_handler.rs; do
  
  echo "=== $f ==="
  
  # 1. pub tenant_id: Uuid -> pub aid: Uuid (struct field)
  sed -i 's/pub tenant_id: Uuid/pub aid: Uuid/g' "$f"
  
  # 2. let tenant_id = Uuid::nil() -> let aid = Uuid::nil()
  sed -i 's/let tenant_id = Uuid::nil()/let aid = Uuid::nil()/g' "$f"
  
  # 3. .bind(tenant_id) -> .bind(aid) for local variables
  sed -i 's/\.bind(tenant_id)/.bind(aid)/g' "$f"
  
  # 4. .bind(&tenant_id) -> .bind(&aid) 
  sed -i 's/\.bind(&tenant_id)/.bind(&aid)/g' "$f"
  
  echo "Done"
done
