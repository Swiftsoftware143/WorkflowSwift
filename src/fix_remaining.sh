#!/bin/bash
# Fix remaining handler files: rename tenant_id variable to aid (keeping SQL column names as tenant_id)

cd /opt/swift/workflowswift

for f in \
  src/handlers/api_key_handler.rs \
  src/handlers/automation_handler.rs \
  src/handlers/brand_monitor_handler.rs \
  src/handlers/bridge_handler.rs \
  src/handlers/client_handler.rs \
  src/handlers/competitor_watch_handler.rs \
  src/handlers/credit_handler.rs \
  src/handlers/dashboard_handler.rs \
  src/handlers/dashboard_tabs_handler.rs \
  src/handlers/incoming_handler.rs \
  src/handlers/instance_handler.rs \
  src/handlers/integration_center_handler.rs \
  src/handlers/integration_dispatch_handler.rs \
  src/handlers/integration_target_handler.rs \
  src/handlers/internal_handler.rs \
  src/handlers/invoice_handler.rs \
  src/handlers/n8n_proxy_handler.rs \
  src/handlers/plan_handler.rs \
  src/handlers/portfolio_handler.rs \
  src/handlers/prospecting_handler.rs \
  src/handlers/provider_keys_handler.rs \
  src/handlers/step_integration_handler.rs \
  src/handlers/tag_handler.rs \
  src/handlers/template_handler.rs \
  src/handlers/user_handler.rs \
  src/handlers/user_integration_handler.rs \
  src/handlers/workflow_handler.rs; do
  
  echo "=== $f ==="
  
  # 1. let tenant_id = Uuid::parse_str -> let aid = Uuid::parse_str (with aid)
  sed -i 's/let tenant_id = Uuid::parse_str/let aid = Uuid::parse_str/g' "$f"
  
  # 2. let _tenant_id = Uuid::parse_str -> let _aid = Uuid::parse_str
  sed -i 's/let _tenant_id = Uuid::parse_str/let _aid = Uuid::parse_str/g' "$f"
  
  # 3. .bind(tenant_id) -> .bind(aid) (local variable)
  sed -i 's/\.bind(tenant_id)/.bind(aid)/g' "$f"
  
  # 4. .bind(&tenant_id) -> .bind(&aid)
  sed -i 's/\.bind(&tenant_id)/.bind(&aid)/g' "$f"
  
  # 5. .bind(tenant_id,) -> .bind(aid,) (just in case)
  sed -i 's/\.bind(tenant_id,)/.bind(aid,)/g' "$f"
  
  # 6. pub tenant_id: Uuid -> pub aid: Uuid (struct fields)
  sed -i 's/pub tenant_id: Uuid/pub aid: Uuid/g' "$f"
  
  # 7. tenant_id: Uuid -> aid: Uuid (struct field without pub)
  sed -i 's/tenant_id: Uuid/aid: Uuid/g' "$f"
  
  echo "Done"
done
