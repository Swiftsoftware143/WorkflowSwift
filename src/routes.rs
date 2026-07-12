use axum::{
    routing::{get, post, put, delete},
    Router,
};

use crate::AppState;
use crate::auth;
use crate::handlers;

pub fn create_router(state: AppState) -> Router {
    // Public auth routes (no auth needed)
    let auth_public = Router::new()
        .route("/login", post(auth::login))
        .route("/register", post(auth::register))
        .route("/forgot-password", post(auth::forgot_password))
        .route("/reset-password", post(auth::reset_password));

    // Protected auth route
    let auth_protected = Router::new()
        .route("/me", get(auth::me))
        .route("/change-password", post(auth::change_password))
        .route("/profile", put(auth::update_profile));

    // Resource sub-routers
    let client_routes = Router::new()
        .route("/", get(handlers::client_handler::list_clients).post(handlers::client_handler::create_client))
        .route("/{id}", get(handlers::client_handler::get_client)
            .put(handlers::client_handler::update_client)
            .delete(handlers::client_handler::delete_client));

    let template_routes = Router::new()
        .route("/", get(handlers::template_handler::list_templates).post(handlers::template_handler::create_template))
        .route("/{id}", get(handlers::template_handler::get_template)
            .put(handlers::template_handler::update_template)
            .delete(handlers::template_handler::delete_template))
        .route("/{id}/steps", get(handlers::template_handler::get_template_steps))
        .route("/{id}/install", post(handlers::template_handler::install_template_as_workflow));

    let workflow_routes = Router::new()
        .route("/", get(handlers::workflow_handler::list_workflows).post(handlers::workflow_handler::create_workflow))
        .route("/{id}", get(handlers::workflow_handler::get_workflow)
            .put(handlers::workflow_handler::update_workflow)
            .delete(handlers::workflow_handler::delete_workflow))
        .route("/{id}/start", post(handlers::workflow_handler::start_workflow))
        .route("/{id}/steps", get(handlers::workflow_handler::get_workflow_steps).post(handlers::workflow_handler::create_workflow_step))
        .route("/{id}/steps/reorder", put(handlers::workflow_handler::reorder_workflow_steps))
        .route("/{id}/steps/{step_id}", put(handlers::workflow_handler::update_workflow_step).delete(handlers::workflow_handler::delete_workflow_step))
        .route("/{id}/deploy", post(handlers::workflow_handler::deploy_workflow_to_n8n))
        .route("/{id}/run", post(handlers::workflow_handler::run_workflow));

    let instance_routes = Router::new()
        .route("/", get(handlers::instance_handler::list_instances))
        .route("/{id}", get(handlers::instance_handler::get_instance)
            .put(handlers::instance_handler::update_instance))
        .route("/{id}/advance", post(handlers::instance_handler::advance_instance))
        .route("/{id}/callback", post(handlers::instance_handler::instance_callback));

    let tag_routes = Router::new()
        .route("/", get(handlers::tag_handler::list_tags).post(handlers::tag_handler::create_tag))
        .route("/assign", post(handlers::tag_handler::assign_tag))
        .route("/unassign", post(handlers::tag_handler::unassign_tag));

    let plan_routes = Router::new()
        .route("/", get(handlers::plan_handler::list_plans).post(handlers::plan_handler::create_plan))
        .route("/capabilities", get(handlers::plan_handler::get_plan_capabilities));

    let credit_routes = Router::new()
        .route("/balance", get(handlers::credit_handler::credit_balance))
        .route("/transactions", get(handlers::credit_handler::list_transactions))
        .route("/packages", get(handlers::credit_handler::list_credit_packages))
        .route("/package", post(handlers::credit_handler::create_credit_package))
        .route("/rollover", post(handlers::credit_handler::rollover_credits))
        .route("/deduct", post(handlers::credit_handler::deduct_credit));

    let automation_routes = Router::new()
        .route("/", get(handlers::automation_handler::list_automations).post(handlers::automation_handler::create_automation))
        .route("/{id}/run", post(handlers::automation_handler::run_automation));

    let tenant_routes = Router::new()
        .route("/", get(handlers::tenant_handler::get_tenant).put(handlers::tenant_handler::update_tenant))
        .route("/hexomatic-key", get(handlers::tenant_handler::get_hexomatic_key).put(handlers::tenant_handler::set_hexomatic_key))
        .route("/industry", get(handlers::industry_handler::get_tenant_industry).put(handlers::industry_handler::set_tenant_industry))
        .route("/industry/{slug}", delete(handlers::industry_handler::remove_tenant_industry));

    let user_routes = Router::new()
        .route("/", get(handlers::user_handler::list_users))
        .route("/invite", post(handlers::user_handler::invite_user));

    let dashboard_routes = Router::new()
        .route("/stats", get(handlers::dashboard_handler::dashboard_stats))
        .route("/activity", get(handlers::dashboard_handler::dashboard_activity))
        .route("/data", post(handlers::dashboard_handler::push_dashboard_data))
        .route("/widgets", get(handlers::industry_handler::get_dashboard_widgets))
        .route("/data/{metric_key}", get(handlers::industry_handler::get_dashboard_metric))
        .route("/push-widget-data", post(handlers::industry_handler::push_widget_data))
        .route("/industry-data", get(handlers::dashboard_handler::industry_dashboard_data))
        .route("/metric-keys", get(handlers::dashboard_handler::get_widget_metric_keys))
        // Dashboard Tabs (Brand Monitor, Competitor Watch, Prospecting)
        .route("/tabs", get(handlers::dashboard_tabs_handler::get_dashboard_tabs))
        .route("/brand-monitor", get(handlers::dashboard_tabs_handler::list_brand_monitors).post(handlers::dashboard_tabs_handler::create_brand_monitor))
        .route("/brand-monitor/{id}", delete(handlers::dashboard_tabs_handler::delete_brand_monitor))
        .route("/brand-monitor/{id}/results", get(handlers::dashboard_tabs_handler::get_brand_monitor_results))
        .route("/competitor-watch", get(handlers::dashboard_tabs_handler::list_competitor_watches).post(handlers::dashboard_tabs_handler::create_competitor_watch))
        .route("/competitor-watch/{id}", delete(handlers::dashboard_tabs_handler::delete_competitor_watch))
        .route("/competitor-watch/{id}/changes", get(handlers::dashboard_tabs_handler::get_competitor_changes))
        .route("/prospecting", get(handlers::dashboard_tabs_handler::list_prospectings).post(handlers::dashboard_tabs_handler::create_prospecting))
        .route("/prospecting/{id}", delete(handlers::dashboard_tabs_handler::delete_prospecting))
        .route("/prospecting/{id}/results", get(handlers::dashboard_tabs_handler::get_prospecting_results))
        .route("/connect-workflow", post(handlers::dashboard_tabs_handler::connect_to_workflow));

    let industry_routes = Router::new()
        .route("/", get(handlers::industry_handler::list_industries));



    let api_key_routes = Router::new()
        .route("/", get(handlers::api_key_handler::list_api_keys).post(handlers::api_key_handler::create_api_key))
        .route("/{id}", put(handlers::api_key_handler::update_api_key).delete(handlers::api_key_handler::delete_api_key));

    let portfolio_routes = Router::new()
        .route("/", get(handlers::portfolio_handler::list_portfolio_companies).post(handlers::portfolio_handler::create_portfolio_company))
        .route("/{id}", get(handlers::portfolio_handler::get_portfolio_company)
            .put(handlers::portfolio_handler::update_portfolio_company)
            .delete(handlers::portfolio_handler::delete_portfolio_company));

    let integration_routes = Router::new()
        .route("/", get(handlers::integration_target_handler::list_integration_targets).post(handlers::integration_target_handler::create_integration_target))
        .route("/{id}", put(handlers::integration_target_handler::update_integration_target).delete(handlers::integration_target_handler::delete_integration_target));

    let dispatch_routes = Router::new()
        .route("/", post(handlers::integration_dispatch_handler::dispatch_integration));

    let step_integration_routes = Router::new()
        .route("/", get(handlers::step_integration_handler::list_step_integrations).post(handlers::step_integration_handler::create_step_integration))
        .route("/{id}", delete(handlers::step_integration_handler::delete_step_integration));

    let available_integration_routes = Router::new()
        .route("/", get(handlers::step_integration_handler::list_available_integrations));

    let _provider_preset_routes = Router::new()
        .route("/", get(handlers::integration_dispatch_handler::list_provider_presets));

    let invoice_routes = Router::new()
        .route("/", get(handlers::invoice_handler::list_invoices))
        .route("/{id}", get(handlers::invoice_handler::get_invoice));

    // Protected routes with auth middleware

    let affiliates_routes = Router::new()
        .route("/", get(handlers::affiliates_handler::list).post(handlers::affiliates_handler::create))
        .route("/{id}", get(handlers::affiliates_handler::get).put(handlers::affiliates_handler::update).delete(handlers::affiliates_handler::delete));

    let leads_routes = Router::new()
        .route("/", get(handlers::leads_handler::list).post(handlers::leads_handler::create))
        .route("/{id}", get(handlers::leads_handler::get).put(handlers::leads_handler::update).delete(handlers::leads_handler::delete));

    let tag_groups_routes = Router::new()
        .route("/", get(handlers::tag_groups_handler::list).post(handlers::tag_groups_handler::create))
        .route("/{id}", get(handlers::tag_groups_handler::get).put(handlers::tag_groups_handler::update).delete(handlers::tag_groups_handler::delete));

    let deals_routes = Router::new()
        .route("/", get(handlers::deals_handler::list).post(handlers::deals_handler::create))
        .route("/{id}", get(handlers::deals_handler::get).put(handlers::deals_handler::update).delete(handlers::deals_handler::delete));

    let campaigns_routes = Router::new()
        .route("/", get(handlers::campaigns_handler::list).post(handlers::campaigns_handler::create))
        .route("/{id}", get(handlers::campaigns_handler::get).put(handlers::campaigns_handler::update).delete(handlers::campaigns_handler::delete));

    let tickets_routes = Router::new()
        .route("/", get(handlers::tickets_handler::list).post(handlers::tickets_handler::create))
        .route("/{id}", get(handlers::tickets_handler::get).put(handlers::tickets_handler::update).delete(handlers::tickets_handler::delete));

    let email_templates_routes = Router::new()
        .route("/", get(handlers::email_templates_handler::list).post(handlers::email_templates_handler::create))
        .route("/{id}", get(handlers::email_templates_handler::get).put(handlers::email_templates_handler::update).delete(handlers::email_templates_handler::delete));

    let webhooks_routes = Router::new()
        .route("/", get(handlers::webhooks_handler::list).post(handlers::webhooks_handler::create))
        .route("/{id}", get(handlers::webhooks_handler::get).put(handlers::webhooks_handler::update).delete(handlers::webhooks_handler::delete));

    let reviews_routes = Router::new()
        .route("/", get(handlers::reviews_handler::list).post(handlers::reviews_handler::create))
        .route("/{id}", get(handlers::reviews_handler::get).put(handlers::reviews_handler::update).delete(handlers::reviews_handler::delete));

    let surfaces_routes = Router::new()
        .route("/", get(handlers::surfaces_handler::list).post(handlers::surfaces_handler::create))
        .route("/{id}", get(handlers::surfaces_handler::get).put(handlers::surfaces_handler::update).delete(handlers::surfaces_handler::delete));

    let categories_routes = Router::new()
        .route("/", get(handlers::categories_handler::list).post(handlers::categories_handler::create))
        .route("/{id}", get(handlers::categories_handler::get).put(handlers::categories_handler::update).delete(handlers::categories_handler::delete));

    let reports_routes = Router::new()
        .route("/", get(handlers::reports_handler::list).post(handlers::reports_handler::create))
        .route("/{id}", get(handlers::reports_handler::get).put(handlers::reports_handler::update).delete(handlers::reports_handler::delete));

    let knowledge_base_routes = Router::new()
        .route("/", get(handlers::knowledge_base_handler::list).post(handlers::knowledge_base_handler::create))
        .route("/{id}", get(handlers::knowledge_base_handler::get).put(handlers::knowledge_base_handler::update).delete(handlers::knowledge_base_handler::delete));

    let call_logs_routes = Router::new()
        .route("/", get(handlers::call_logs_handler::list).post(handlers::call_logs_handler::create))
        .route("/{id}", get(handlers::call_logs_handler::get).put(handlers::call_logs_handler::update).delete(handlers::call_logs_handler::delete));

    let calendar_events_routes = Router::new()
        .route("/", get(handlers::calendar_events_handler::list).post(handlers::calendar_events_handler::create))
        .route("/{id}", get(handlers::calendar_events_handler::get).put(handlers::calendar_events_handler::update).delete(handlers::calendar_events_handler::delete));
    let import_logs_routes = Router::new()
        .route("/", get(handlers::import_logs_handler::list).post(handlers::import_logs_handler::create))
        .route("/{id}", get(handlers::import_logs_handler::get).put(handlers::import_logs_handler::update).delete(handlers::import_logs_handler::delete));

    let export_templates_routes = Router::new()
        .route("/", get(handlers::export_templates_handler::list).post(handlers::export_templates_handler::create))
        .route("/{id}", get(handlers::export_templates_handler::get).put(handlers::export_templates_handler::update).delete(handlers::export_templates_handler::delete));

    let brand_monitor_routes = Router::new()
        .route("/", get(handlers::brand_monitor_handler::list_brand_monitors).post(handlers::brand_monitor_handler::create_brand_monitor))
        .route("/{id}", delete(handlers::brand_monitor_handler::delete_brand_monitor))
        .route("/search", post(handlers::brand_monitor_handler::search_brand_mentions));

    let competitor_routes = Router::new()
        .route("/", get(handlers::competitor_watch_handler::list_competitors).post(handlers::competitor_watch_handler::create_competitor))
        .route("/{id}", put(handlers::competitor_watch_handler::update_competitor).delete(handlers::competitor_watch_handler::delete_competitor))
        .route("/{id}/check", post(handlers::competitor_watch_handler::check_competitor));

    let prospecting_routes = Router::new()
        .route("/search", post(handlers::prospecting_handler::search_businesses))
        .route("/enrich", post(handlers::prospecting_handler::enrich_business));

    let bridge_routes = Router::new()
        .route("/ingest", post(handlers::bridge_handler::ingest_data))
        .route("/commands", get(handlers::bridge_handler::get_commands));

    let n8n_routes = Router::new()
        .route("/trigger", post(handlers::n8n_proxy_handler::trigger_n8n_workflow))
        .route("/health", get(handlers::n8n_proxy_handler::check_n8n_health));

    let provider_keys_routes = Router::new()
        .route("/", get(handlers::provider_keys_handler::list_provider_keys)
            .post(handlers::provider_keys_handler::upsert_provider_key))
        .route("/{provider}", delete(handlers::provider_keys_handler::delete_provider_key));

    let integration_center_routes = Router::new()
        .route("/", get(handlers::integration_center_handler::get_destinations))
        .route("/values", get(handlers::integration_center_handler::get_destination_values));

    let user_integration_routes = Router::new()
        .route("/", get(handlers::user_integration_handler::list_integrations).post(handlers::user_integration_handler::upsert_integration))
        .route("/native", get(handlers::user_integration_handler::list_native_integrations))
        .route("/native/{provider}", post(handlers::user_integration_handler::toggle_native_integration))
        .route("/{provider}", delete(handlers::user_integration_handler::delete_integration))
        .route("/resolve", get(handlers::user_integration_handler::resolve_step_provider))
        .route("/health-check", post(handlers::user_integration_handler::check_integration_health));

    let user_key_routes = Router::new()
        .route("/", get(handlers::integration_center_handler::list_user_keys))
        .route("/generate", post(handlers::integration_center_handler::generate_user_key))
        .route("/{id}", delete(handlers::integration_center_handler::revoke_user_key))
        .route("/health-check", post(handlers::integration_center_handler::check_provider_health));

    let protected_routes = Router::new()
        .nest("/auth", auth_protected)
        .nest("/tenants", tenant_routes)
        .nest("/users", user_routes)
        .nest("/clients", client_routes)
        .nest("/templates", template_routes)
        .nest("/workflows", workflow_routes)
        .nest("/instances", instance_routes)
        .nest("/tags", tag_routes)
        .nest("/plans", plan_routes)
        .nest("/credits", credit_routes)
        .nest("/automations", automation_routes)
        .nest("/dashboard", dashboard_routes)
        .nest("/industries", industry_routes)
        .nest("/api-keys", api_key_routes)
        .nest("/portfolio-companies", portfolio_routes)
        .nest("/integration-targets", integration_routes)
        .nest("/integration-dispatch", dispatch_routes)
        .nest("/step-integrations", step_integration_routes)
        .nest("/available-integrations", available_integration_routes)
        .nest("/invoices", invoice_routes)
        .nest("/affiliates", affiliates_routes)
        .nest("/leads", leads_routes)
        .nest("/tag-groups", tag_groups_routes)
        .nest("/deals", deals_routes)
        .nest("/campaigns", campaigns_routes)
        .nest("/tickets", tickets_routes)
        .nest("/email-templates", email_templates_routes)
        .nest("/webhooks", webhooks_routes)
        .nest("/reviews", reviews_routes)
        .nest("/surfaces", surfaces_routes)
        .nest("/categories", categories_routes)
        .nest("/reports", reports_routes)
        .nest("/knowledge-base", knowledge_base_routes)
        .nest("/call-logs", call_logs_routes)
        .nest("/calendar-events", calendar_events_routes)
        .nest("/import-logs", import_logs_routes)
        .nest("/export-templates", export_templates_routes)
        .nest("/brand-monitors", brand_monitor_routes)
        .nest("/competitors", competitor_routes)
        .nest("/prospecting", prospecting_routes)
        .nest("/bridge", bridge_routes)
        .nest("/n8n", n8n_routes)
        .nest("/provider-keys", provider_keys_routes)
        .nest("/integration-destinations", integration_center_routes)
        .nest("/integrations", user_integration_routes)
        .nest("/user-keys", user_key_routes)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    // Public routes (no auth)
    let public_routes = Router::new()
        .nest("/auth", auth_public)
        .route("/health", get(health_check))
        .route("/internal/portfolio-companies", post(handlers::portfolio_handler::internal_create_portfolio_company))
        .route("/internal/dashboard-data-seed", post(handlers::internal_handler::seed_dashboard_data))
        .route("/provider-presets", get(handlers::integration_dispatch_handler::list_provider_presets))
        .route("/available-providers", get(handlers::provider_keys_handler::list_available_providers))
        .route("/incoming", post(handlers::incoming_handler::receive_incoming));

    // Combine: public + protected merged
    let api_v1 = Router::new()
        .merge(public_routes)
        .merge(protected_routes);

    Router::new()
        .nest("/api/v1", api_v1)
        .with_state(state)
}

async fn health_check() -> impl axum::response::IntoResponse {
    (axum::http::StatusCode::OK, axum::Json(serde_json::json!({
        "status": "ok",
        "service": "workflowswift-api",
        "version": env!("CARGO_PKG_VERSION")
    })))
}
