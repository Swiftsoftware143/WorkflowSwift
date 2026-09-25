use axum::{
    routing::{delete, get, post, put},
    Router,
};

use crate::auth;
use crate::handlers;
use crate::AppState;

pub fn create_router(state: AppState) -> Router {
    // Body-read deadline (kanban t_e7cba83e): one value, read once, mounted on the routes that
    // read a request body. See `crate::rate_limit::body_read_deadline_middleware` for why the
    // bound is on the body's arrival and not on the handler.
    let body_read_deadline =
        crate::rate_limit::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs);

    // Public auth routes (no auth needed)
    let auth_public = Router::new()
        .route("/login", post(auth::login))
        .route("/register", post(auth::register))
        .route("/forgot-password", post(auth::forgot_password))
        .route("/reset-password", post(auth::reset_password))
        // Load shedding, not rate limiting: these four are the only routes a stranger can
        // reach that do Argon2 work (a 19 MiB hash) or send mail, and the Argon2 semaphore
        // bounds the *work* without bounding the *queue* — requests that miss a hashing permit
        // wait, each holding a task, a connection and a body buffer, with no cap on how many
        // (kanban t_92bafdf8). This admits at most AUTH_IN_FLIGHT_CAP concurrent requests into
        // the four and answers 429 immediately to anything above, so the queue can no longer
        // grow and legitimate traffic is never parked behind a flood. It is mounted here, on
        // the public auth routes only: /health, the webhooks and every other public route are
        // untouched.
        .layer(axum::middleware::from_fn_with_state(
            state.auth_in_flight.clone(),
            crate::rate_limit::password_auth_shed_middleware,
        ));

    // Protected auth route
    let auth_protected = Router::new()
        .route("/me", get(auth::me))
        .route("/me/usage", get(auth::get_usage))
        .route("/change-password", post(auth::change_password))
        .route("/profile", put(auth::update_profile));

    // Resource sub-routers

    let template_routes = Router::new()
        .route(
            "/",
            get(handlers::template_handler::list_templates)
                .post(handlers::template_handler::create_template),
        )
        .route(
            "/{id}",
            get(handlers::template_handler::get_template)
                .put(handlers::template_handler::update_template)
                .delete(handlers::template_handler::delete_template),
        )
        .route(
            "/{id}/steps",
            get(handlers::template_handler::get_template_steps),
        )
        .route(
            "/{id}/install",
            post(handlers::template_handler::install_template_as_workflow),
        )
        .route(
            "/{id}/export",
            get(handlers::template_handler::export_template),
        )
        .route("/import", post(handlers::template_handler::import_template));

    let workflow_routes = Router::new()
        .route(
            "/",
            get(handlers::workflow_handler::list_workflows)
                .post(handlers::workflow_handler::create_workflow),
        )
        .route(
            "/{id}",
            get(handlers::workflow_handler::get_workflow)
                .put(handlers::workflow_handler::update_workflow)
                .delete(handlers::workflow_handler::delete_workflow),
        )
        .route(
            "/{id}/start",
            post(handlers::workflow_handler::start_workflow),
        )
        .route(
            "/{id}/steps",
            get(handlers::workflow_handler::get_workflow_steps)
                .post(handlers::workflow_handler::create_workflow_step),
        )
        .route(
            "/{id}/steps/reorder",
            put(handlers::workflow_handler::reorder_workflow_steps),
        )
        .route(
            "/{id}/steps/{step_id}",
            put(handlers::workflow_handler::update_workflow_step)
                .delete(handlers::workflow_handler::delete_workflow_step),
        )
        .route(
            "/{id}/deploy",
            post(handlers::workflow_handler::deploy_workflow_to_n8n),
        )
        .route("/{id}/run", post(handlers::workflow_handler::run_workflow))
        .route(
            "/validate-steps",
            post(handlers::workflow_handler::validate_workflow_steps),
        )
        // The shipped Swift Market Intel extension posts here (background.js /
        // popup.js) with {workflow_id, input_data, source, timestamp} — without
        // this route the request was swallowed by /{id} and answered 405.
        .route(
            "/trigger",
            post(handlers::n8n_proxy_handler::trigger_extension_workflow),
        );

    let instance_routes = Router::new()
        .route("/", get(handlers::instance_handler::list_instances))
        .route(
            "/{id}",
            get(handlers::instance_handler::get_instance)
                .put(handlers::instance_handler::update_instance),
        )
        .route(
            "/{id}/advance",
            post(handlers::instance_handler::advance_instance),
        )
        // The run history of one instance (workflow_execution_logs), tenant-scoped.
        .route(
            "/{id}/logs",
            get(handlers::instance_handler::list_instance_logs),
        )
        // Approve/reject a waiting step (manual/approval gate, or fast-forward a
        // delay). Without this a gate was a dead end: the run stopped and nothing
        // could move it (kanban t_1ff4b916 item 2).
        .route(
            "/{id}/steps/{step_id}/decision",
            post(handlers::instance_handler::decide_instance_step),
        );
    // NOTE: `/{id}/callback` is NOT here. n8n calls it with X-Internal-Key and no
    // JWT, so it lives on the public router and authenticates itself — see
    // instance_handler::instance_callback (kanban t_a6a1b299 item 4).

    let tag_routes = Router::new()
        .route(
            "/",
            get(handlers::tag_handler::list_tags).post(handlers::tag_handler::create_tag),
        )
        .route("/assign", post(handlers::tag_handler::assign_tag))
        .route("/unassign", post(handlers::tag_handler::unassign_tag));

    let plan_routes = Router::new()
        .route(
            "/",
            get(handlers::plan_handler::list_plans).post(handlers::plan_handler::create_plan),
        )
        .route(
            "/capabilities",
            get(handlers::plan_handler::get_plan_capabilities),
        );

    let credit_routes = Router::new()
        .route("/balance", get(handlers::credit_handler::credit_balance))
        .route(
            "/transactions",
            get(handlers::credit_handler::list_transactions),
        )
        .route(
            "/packages",
            get(handlers::credit_handler::list_credit_packages),
        )
        .route(
            "/package",
            post(handlers::credit_handler::create_credit_package),
        )
        .route(
            "/rollover",
            post(handlers::credit_handler::rollover_credits),
        )
        .route("/deduct", post(handlers::credit_handler::deduct_credit));

    let automation_routes = Router::new()
        .route(
            "/",
            get(handlers::automation_handler::list_automations)
                .post(handlers::automation_handler::create_automation),
        )
        .route(
            "/{id}/run",
            post(handlers::automation_handler::run_automation),
        );

    let account_routes = Router::new()
        .route(
            "/",
            get(handlers::account_handler::get_account)
                .put(handlers::account_handler::update_account),
        )
        .route(
            "/hexomatic-key",
            get(handlers::account_handler::get_hexomatic_key)
                .put(handlers::account_handler::set_hexomatic_key),
        )
        .route(
            "/industry",
            get(handlers::industry_handler::get_account_industry)
                .put(handlers::industry_handler::set_account_industry),
        )
        .route(
            "/industry/{slug}",
            delete(handlers::industry_handler::remove_account_industry),
        );

    let user_routes = Router::new()
        .route("/", get(handlers::user_handler::list_users))
        .route("/invite", post(handlers::user_handler::invite_user))
        .route("/team", get(handlers::user_handler::list_team_members))
        .route("/{id}", delete(handlers::user_handler::remove_user))
        .route(
            "/{id}/permissions",
            put(handlers::user_handler::update_user_permissions),
        )
        .route("/{id}/role", put(handlers::user_handler::set_user_role));

    // ── Workspace routes ──
    let workspace_routes = Router::new()
        .route("/", get(handlers::workspace_handler::list_user_workspaces))
        .route(
            "/",
            post(handlers::workspace_handler::create_user_workspace),
        )
        .route(
            "/{id}",
            delete(handlers::workspace_handler::delete_user_workspace),
        )
        .route(
            "/{id}/stats",
            get(handlers::workspace_handler::get_workspace_stats),
        );
    // NOTE (kanban t_f42d6527): five `/{id}/…` registrations used to sit here — GET `/{id}/agents`,
    // GET `/{id}/tickets`, PATCH `/{id}/tickets/{ticket_id}/status`, GET+POST `/{id}/provider-keys`
    // and DELETE `/{id}/provider-keys/{provider}`. They are DELETED, per route:
    //   * no served surface called any of them (`grep -rn` over www-app/, www-admin/ and the
    //     marketing www/ root = 0 hits, and each repo copy is byte-identical to the served one),
    //   * their `{id}` path segment was never read by any handler (all of them take
    //     `?workspace_id=`), so they never did what their path claimed,
    //   * each duplicated a live route: GET /api/v1/agents is the SAME handler (agent_routes below),
    //     GET/POST/DELETE /api/v1/provider-keys is the richer implementation all three served
    //     surfaces use, and GET /api/v1/tickets serves support tickets from `tickets` — the deleted
    //     one read `agent_tickets`, which no code in this app ever INSERTs.
    // The correct way to ask for one workspace's agents is GET /api/v1/agents?workspace_id={id}.

    let agent_routes = Router::new()
        .route(
            "/",
            get(handlers::agent_handler::list_agents).post(handlers::agent_handler::create_agent),
        )
        .route("/{id}", delete(handlers::agent_handler::delete_agent));

    let dashboard_routes = Router::new()
        .route(
            "/workspace",
            get(handlers::paperclip_handler::workspace_dashboard),
        )
        .route(
            "/timeline",
            get(handlers::paperclip_handler::activity_timeline),
        )
        .route("/stats", get(handlers::dashboard_handler::dashboard_stats))
        .route(
            "/activity",
            get(handlers::dashboard_handler::dashboard_activity),
        )
        .route(
            "/data",
            post(handlers::dashboard_handler::push_dashboard_data),
        )
        .route(
            "/widgets",
            get(handlers::industry_handler::get_dashboard_widgets),
        )
        .route(
            "/push-widget-data",
            post(handlers::industry_handler::push_widget_data),
        )
        // Dashboard Tabs (Brand Monitor, Competitor Watch, Prospecting)
        .route(
            "/tabs",
            get(handlers::dashboard_tabs_handler::get_dashboard_tabs),
        )
        .route(
            "/brand-monitor",
            get(handlers::dashboard_tabs_handler::list_brand_monitors)
                .post(handlers::dashboard_tabs_handler::create_brand_monitor),
        )
        .route(
            "/brand-monitor/{id}",
            delete(handlers::dashboard_tabs_handler::delete_brand_monitor),
        )
        .route(
            "/brand-monitor/{id}/results",
            get(handlers::dashboard_tabs_handler::get_brand_monitor_results),
        )
        .route(
            "/competitor-watch",
            get(handlers::dashboard_tabs_handler::list_competitor_watches)
                .post(handlers::dashboard_tabs_handler::create_competitor_watch),
        )
        .route(
            "/competitor-watch/{id}",
            delete(handlers::dashboard_tabs_handler::delete_competitor_watch),
        )
        .route(
            "/competitor-watch/{id}/changes",
            get(handlers::dashboard_tabs_handler::get_competitor_changes),
        )
        .route(
            "/prospecting",
            get(handlers::dashboard_tabs_handler::list_prospectings)
                .post(handlers::dashboard_tabs_handler::create_prospecting),
        )
        .route(
            "/prospecting/{id}",
            delete(handlers::dashboard_tabs_handler::delete_prospecting),
        )
        .route(
            "/prospecting/{id}/results",
            get(handlers::dashboard_tabs_handler::get_prospecting_results),
        )
        .route(
            "/connect-workflow",
            post(handlers::dashboard_tabs_handler::connect_to_workflow),
        );

    let api_key_routes = Router::new()
        .route(
            "/",
            get(handlers::api_key_handler::list_api_keys)
                .post(handlers::api_key_handler::create_api_key),
        )
        .route(
            "/{id}",
            put(handlers::api_key_handler::update_api_key)
                .delete(handlers::api_key_handler::delete_api_key),
        );

    let portfolio_routes = Router::new()
        .route(
            "/",
            get(handlers::portfolio_handler::list_portfolio_companies)
                .post(handlers::portfolio_handler::create_portfolio_company),
        )
        .route(
            "/{id}",
            get(handlers::portfolio_handler::get_portfolio_company)
                .put(handlers::portfolio_handler::update_portfolio_company)
                .delete(handlers::portfolio_handler::delete_portfolio_company),
        );

    let integration_routes = Router::new()
        .route(
            "/",
            get(handlers::integration_target_handler::list_integration_targets)
                .post(handlers::integration_target_handler::create_integration_target),
        )
        .route(
            "/{id}",
            put(handlers::integration_target_handler::update_integration_target)
                .delete(handlers::integration_target_handler::delete_integration_target),
        );

    let dispatch_routes = Router::new().route(
        "/",
        post(handlers::integration_dispatch_handler::dispatch_integration),
    );

    let step_integration_routes = Router::new()
        .route(
            "/",
            get(handlers::step_integration_handler::list_step_integrations)
                .post(handlers::step_integration_handler::create_step_integration),
        )
        .route(
            "/{id}",
            delete(handlers::step_integration_handler::delete_step_integration),
        );

    let available_integration_routes = Router::new().route(
        "/",
        get(handlers::step_integration_handler::list_available_integrations),
    );

    let invoice_routes = Router::new()
        .route("/", get(handlers::invoice_handler::list_invoices))
        .route("/{id}", get(handlers::invoice_handler::get_invoice));

    // Protected routes with auth middleware

    let leads_routes = Router::new()
        .route(
            "/",
            get(handlers::leads_handler::list).post(handlers::leads_handler::create),
        )
        .route(
            "/{id}",
            get(handlers::leads_handler::get)
                .put(handlers::leads_handler::update)
                .delete(handlers::leads_handler::delete),
        );

    let tag_groups_routes = Router::new()
        .route(
            "/",
            get(handlers::tag_groups_handler::list).post(handlers::tag_groups_handler::create),
        )
        .route(
            "/{id}",
            get(handlers::tag_groups_handler::get)
                .put(handlers::tag_groups_handler::update)
                .delete(handlers::tag_groups_handler::delete),
        );

    let tickets_routes = Router::new()
        .route(
            "/",
            get(handlers::tickets_handler::list).post(handlers::tickets_handler::create),
        )
        .route(
            "/{id}",
            get(handlers::tickets_handler::get)
                .put(handlers::tickets_handler::update)
                .delete(handlers::tickets_handler::delete),
        );

    let email_templates_routes = Router::new()
        .route(
            "/",
            get(handlers::email_templates_handler::list)
                .post(handlers::email_templates_handler::create),
        )
        .route(
            "/{id}",
            get(handlers::email_templates_handler::get)
                .put(handlers::email_templates_handler::update)
                .delete(handlers::email_templates_handler::delete),
        );

    let webhooks_routes = Router::new()
        .route(
            "/",
            get(handlers::webhooks_handler::list).post(handlers::webhooks_handler::create),
        )
        .route(
            "/{id}",
            get(handlers::webhooks_handler::get)
                .put(handlers::webhooks_handler::update)
                .delete(handlers::webhooks_handler::delete),
        );

    let surfaces_routes = Router::new()
        .route(
            "/",
            get(handlers::surfaces_handler::list).post(handlers::surfaces_handler::create),
        )
        .route(
            "/{id}",
            get(handlers::surfaces_handler::get)
                .put(handlers::surfaces_handler::update)
                .delete(handlers::surfaces_handler::delete),
        );

    let brand_monitor_routes = Router::new()
        .route(
            "/",
            get(handlers::brand_monitor_handler::list_brand_monitors)
                .post(handlers::brand_monitor_handler::create_brand_monitor),
        )
        .route(
            "/{id}",
            delete(handlers::brand_monitor_handler::delete_brand_monitor),
        )
        .route(
            "/search",
            post(handlers::brand_monitor_handler::search_brand_mentions),
        );

    let competitor_routes = Router::new()
        .route(
            "/",
            get(handlers::competitor_watch_handler::list_competitors)
                .post(handlers::competitor_watch_handler::create_competitor),
        )
        .route(
            "/{id}",
            put(handlers::competitor_watch_handler::update_competitor)
                .delete(handlers::competitor_watch_handler::delete_competitor),
        )
        .route(
            "/{id}/check",
            post(handlers::competitor_watch_handler::check_competitor),
        );

    let prospecting_routes = Router::new()
        .route(
            "/search",
            post(handlers::prospecting_handler::search_businesses),
        )
        .route(
            "/enrich",
            post(handlers::prospecting_handler::enrich_business),
        );

    let bridge_routes = Router::new()
        .route("/ingest", post(handlers::bridge_handler::ingest_data))
        .route("/commands", get(handlers::bridge_handler::get_commands))
        .route(
            "/commands/ack",
            post(handlers::bridge_handler::acknowledge_command),
        )
        .route("/status", get(handlers::bridge_handler::bridge_status));

    let n8n_routes = Router::new()
        .route(
            "/trigger",
            post(handlers::n8n_proxy_handler::trigger_n8n_workflow),
        )
        .route(
            "/health",
            get(handlers::n8n_proxy_handler::check_n8n_health),
        );

    let provider_keys_routes = Router::new()
        .route(
            "/",
            get(handlers::provider_keys_handler::list_provider_keys)
                .post(handlers::provider_keys_handler::upsert_provider_key),
        )
        .route(
            "/{provider}",
            delete(handlers::provider_keys_handler::delete_provider_key),
        )
        .route(
            "/{provider}/test",
            post(handlers::coreswift_integration_handler::test_provider_key),
        );

    let integration_center_routes = Router::new()
        .route(
            "/",
            get(handlers::integration_center_handler::get_destinations),
        )
        .route(
            "/values",
            get(handlers::integration_center_handler::get_destination_values),
        );

    let user_integration_routes = Router::new()
        .route(
            "/",
            get(handlers::user_integration_handler::list_integrations)
                .post(handlers::user_integration_handler::upsert_integration),
        )
        .route(
            "/native",
            get(handlers::user_integration_handler::list_native_integrations),
        )
        .route(
            "/native/{provider}",
            post(handlers::user_integration_handler::toggle_native_integration),
        )
        // ── Canonical CoreSwift spoke (fleet standard paths) ──
        .route(
            "/coreswift/status",
            get(handlers::coreswift_integration_handler::coreswift_status),
        )
        .route(
            "/coreswift/lists",
            get(handlers::coreswift_integration_handler::coreswift_lists),
        )
        .route(
            "/coreswift/push",
            post(handlers::coreswift_integration_handler::coreswift_push),
        )
        .route(
            "/{provider}",
            delete(handlers::user_integration_handler::delete_integration),
        )
        .route(
            "/resolve",
            get(handlers::user_integration_handler::resolve_step_provider),
        )
        .route(
            "/health-check",
            post(handlers::user_integration_handler::check_integration_health),
        );

    let rendition_routes = Router::new()
        .route(
            "/",
            get(handlers::rendition_handler::list_renditions)
                .post(handlers::rendition_handler::create_rendition),
        )
        .route(
            "/{id}",
            get(handlers::rendition_handler::get_rendition)
                .put(handlers::rendition_handler::update_rendition)
                .delete(handlers::rendition_handler::delete_rendition),
        )
        .route(
            "/summary",
            get(handlers::rendition_handler::rendition_summary),
        )
        .route(
            "/purge-expired",
            post(handlers::rendition_handler::purge_expired_renditions),
        )
        .route(
            "/stitch",
            post(handlers::rendition_handler::create_stitch_group),
        )
        .route(
            "/workflow/{workflow_id}",
            get(handlers::rendition_handler::list_workflow_renditions),
        );

    let provider_category_routes = Router::new()
        .route(
            "/",
            get(handlers::rendition_handler::list_provider_categories),
        )
        .route(
            "/{category}",
            get(handlers::rendition_handler::get_category_providers),
        );

    let step_type_category_routes = Router::new().route(
        "/{category}",
        get(handlers::rendition_handler::get_step_types_for_category),
    );

    let user_key_routes = Router::new()
        .route(
            "/",
            get(handlers::integration_center_handler::list_user_keys),
        )
        .route(
            "/generate",
            post(handlers::integration_center_handler::generate_user_key),
        )
        .route(
            "/{id}",
            delete(handlers::integration_center_handler::revoke_user_key),
        )
        .route(
            "/health-check",
            post(handlers::integration_center_handler::check_provider_health),
        );

    // ── Admin routes (protected by admin middleware) ──
    let admin_routes = Router::new()
        // Usage dashboard
        .route(
            "/usage",
            get(handlers::admin_settings_handler::admin_usage_dashboard),
        )
        // Settings
        .route(
            "/settings",
            get(handlers::admin_settings_handler::list_settings),
        )
        .route(
            "/settings/email/test",
            post(handlers::admin_settings_handler::test_email_settings),
        )
        .route(
            "/settings/{key}",
            get(handlers::admin_settings_handler::get_setting)
                .put(handlers::admin_settings_handler::update_setting),
        )
        // Retention policy
        .route(
            "/retention",
            get(handlers::admin_settings_handler::get_retention_policy)
                .put(handlers::admin_settings_handler::update_retention_policy),
        )
        // Feature definitions
        .route(
            "/feature-definitions",
            get(handlers::admin_settings_handler::list_feature_definitions),
        )
        // Plan management (full admin CRUD)
        .route(
            "/plans",
            get(handlers::admin_settings_handler::admin_list_plans)
                .post(handlers::admin_settings_handler::admin_create_plan),
        )
        .route(
            "/plans/{id}",
            put(handlers::admin_settings_handler::admin_update_plan_full)
                .delete(handlers::admin_settings_handler::admin_delete_plan),
        )
        // Account management
        .route(
            "/accounts",
            get(handlers::admin_settings_handler::admin_list_accounts),
        )
        .route(
            "/accounts/{id}",
            delete(handlers::admin_settings_handler::admin_delete_account),
        )
        .route(
            "/accounts/create",
            post(handlers::admin_settings_handler::admin_create_account),
        )
        .route(
            "/accounts/{id}/retention",
            put(handlers::admin_settings_handler::admin_set_account_retention),
        )
        // Entitlement: moving an account onto a plan. The only supported path — there is no
        // self-serve checkout (`billing.enabled = false`), and without this an account stuck on
        // a tier that denies `api_access` could never be moved off it.
        .route(
            "/accounts/{id}/plan",
            put(handlers::admin_settings_handler::admin_assign_plan),
        )
        // Email templates (admin-only CRUD)
        .route(
            "/email-templates",
            get(handlers::admin_settings_handler::admin_list_email_templates)
                .post(handlers::admin_settings_handler::admin_create_email_template),
        )
        .route(
            "/email-templates/{id}",
            put(handlers::admin_settings_handler::admin_update_email_template)
                .delete(handlers::admin_settings_handler::admin_delete_email_template),
        )
        // Industry Source Management
        .route(
            "/industry-sources",
            get(handlers::industry_sources_handler::list_industry_sources)
                .post(handlers::industry_sources_handler::upsert_industry_source),
        )
        .route(
            "/industry-sources/seed",
            post(handlers::industry_sources_handler::seed_industry_sources),
        )
        .route(
            "/industry-sources/{id}",
            delete(handlers::industry_sources_handler::delete_industry_source),
        )
        .route(
            "/site",
            get(handlers::site_handler::get_site).put(handlers::site_handler::update_site),
        );

    let protected_routes = Router::new()
        .nest("/auth", auth_protected)
        .nest("/accounts", account_routes)
        .nest("/users", user_routes)
        .nest("/templates", template_routes)
        .nest("/workflows", workflow_routes)
        .nest("/instances", instance_routes)
        .nest("/tags", tag_routes)
        .nest("/plans", plan_routes)
        .nest("/credits", credit_routes)
        .nest("/workspaces", workspace_routes)
        .nest("/agents", agent_routes)
        .nest("/automations", automation_routes)
        .nest("/dashboard", dashboard_routes)
        .nest("/api-keys", api_key_routes)
        .nest("/portfolio-companies", portfolio_routes)
        .nest("/integration-targets", integration_routes)
        .nest("/integration-dispatch", dispatch_routes)
        .nest("/step-integrations", step_integration_routes)
        .nest("/available-integrations", available_integration_routes)
        .nest("/invoices", invoice_routes)
        .nest("/leads", leads_routes)
        .nest("/tag-groups", tag_groups_routes)
        .nest("/tickets", tickets_routes)
        .nest("/email-templates", email_templates_routes)
        .nest("/webhooks", webhooks_routes)
        .nest("/surfaces", surfaces_routes)
        .nest("/brand-monitors", brand_monitor_routes)
        .nest("/competitors", competitor_routes)
        .nest("/prospecting", prospecting_routes)
        .nest("/bridge", bridge_routes)
        .nest("/n8n", n8n_routes)
        .nest("/provider-keys", provider_keys_routes)
        .nest("/integration-destinations", integration_center_routes)
        .nest("/integrations", user_integration_routes)
        .nest("/renditions", rendition_routes)
        .nest("/provider-categories", provider_category_routes)
        .nest("/step-types", step_type_category_routes)
        .nest("/user-keys", user_key_routes)
        .route(
            "/impersonate",
            post(crate::handlers::admin_settings_handler::admin_impersonate),
        )
        .route(
            "/stop-impersonation",
            post(crate::handlers::admin_settings_handler::admin_stop_impersonation),
        )
        // Checkout / Payment routes
        .route(
            "/checkout/create",
            post(handlers::checkout_handler::create_checkout_session),
        )
        .route(
            "/checkout/sessions",
            get(handlers::checkout_handler::list_checkout_sessions),
        )
        .route(
            "/payment-providers",
            get(handlers::checkout_handler::list_payment_providers)
                .post(handlers::checkout_handler::upsert_payment_provider),
        )
        .route(
            "/payment-providers/{provider_type}",
            delete(handlers::checkout_handler::delete_payment_provider),
        )
        .nest("/admin", admin_routes)
        // Layer order: in axum the LAST `.layer()` is the OUTERMOST, so the request path is
        // pre_auth_rate_limit -> auth -> per-account rate limit -> handler.
        //
        // The pre-auth limiter is first on purpose: `rate_limit_middleware` needs `Claims`,
        // which `auth_middleware` only produces after it has verified the credential — and
        // for an API key that verification is an Argon2 check over stored hashes. With auth
        // outermost, an unauthenticated caller could drive that work at full request rate
        // and only be throttled after the fact (measured: ~100 ms of CPU per request that
        // merely looked like an API key). Throttling now happens before any credential is
        // touched.
        // Body-read deadline (kanban t_e7cba83e), INNERMOST on purpose. This `.layer()` is the
        // first of the four, and in axum the first is the innermost, so every credential guard
        // below sits OUTSIDE it: an unauthenticated caller is still answered 401 by the pre-auth
        // limiter / auth layer without the body being read, exactly as before. The deadline
        // starts only once a request has passed those checks and its handler is about to read the
        // body it declared — which is the unbounded hold this closes for authenticated callers
        // too.
        .layer(axum::middleware::from_fn_with_state(
            body_read_deadline,
            crate::rate_limit::body_read_deadline_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::rate_limit::rate_limit_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::rate_limit::pre_auth_rate_limit_middleware,
        ));

    // Public routes that READ a request body, and therefore carry the body-read deadline
    // (kanban t_e7cba83e).
    //
    // These eight are exactly the public routes whose handler cannot answer before the body has
    // been read: `Json` / `Bytes` extractors run before the handler body, so the `X-Internal-Key`
    // / signature check inside `instance_callback`, the `internal/*` handlers, `/incoming`,
    // `stripe_webhook` and `paypal_webhook` all happen *after* the body arrives. A client that
    // sends a request head and then stops used to park a task, a connection and a partially-read
    // body buffer for ever on each of them. **A new public route that reads a body belongs in
    // this sub-router** — that is the maintenance rule, because the layer is mounted here rather
    // than on the whole public router for two measured reasons:
    //
    //   * `/health`, `/internal/dashboard-data-seed` (which reads no body and so answers its key
    //     check immediately) and the GET listing routes answer without any body read at all;
    //     mounting the deadline over them would make them wait for a body they never wanted —
    //     turning a fast 200/401 into a 30 s 408.
    //   * the four password-auth routes are nested separately and keep their own, stricter bound
    //     (the in-flight ceiling's 15 s 429, `b1b93a4`), which the deadline must not pre-empt.
    let public_body_routes = Router::new()
        // n8n writes its execution result back here. Self-authenticating: either
        // X-Internal-Key (n8n) or a user JWT scoped to the instance's account.
        .route(
            "/instances/{id}/callback",
            post(handlers::instance_handler::instance_callback),
        )
        .route(
            "/internal/portfolio-companies",
            post(handlers::portfolio_handler::internal_create_portfolio_company),
        )
        .route(
            "/internal/portfolio-sync",
            post(handlers::portfolio_sync_handler::portfolio_sync_internal),
        )
        // NOTE (card t_79d7d1d2): /internal/tag-provision is deliberately NOT a route here.
        // Its handler file (handlers/tag_provision_handler.rs, which wrote into `clients`/`accounts`
        // keyed off "the oldest account in the DB") was never declared in handlers/mod.rs, so the
        // route answered 404 for every caller since the module declaration was lost, and nothing
        // called it: 0 n8n workflows, 0 fleet apps, 0 served shells, 0 shell scripts. Deleted
        // 2026-09-25 rather than wired — a webhook would have injected leads into an arbitrary
        // tenant, and cross-app tag/lead provisioning belongs to the CRM hub (CoreSwift-CRM),
        // which owns a real /api/v1/internal/tag-provision. Do not re-add one here without a caller.
        .route(
            "/internal/tags/assign",
            post(handlers::internal_handler::internal_assign_tag),
        )
        .route(
            "/internal/tags/delete",
            post(handlers::internal_handler::internal_remove_tag),
        )
        .route(
            "/incoming",
            post(handlers::incoming_handler::receive_incoming),
        )
        // Public webhooks (no auth — signature verification is done in the handler)
        .route(
            "/webhooks/stripe",
            post(handlers::checkout_handler::stripe_webhook),
        )
        .route(
            "/webhooks/paypal",
            post(handlers::checkout_handler::paypal_webhook),
        )
        .layer(axum::middleware::from_fn_with_state(
            body_read_deadline,
            crate::rate_limit::body_read_deadline_middleware,
        ));

    // Public routes (no auth) that do not read a body, plus the bounded password-auth routes.
    let public_routes = Router::new()
        .nest("/auth", auth_public)
        .route("/health", get(health_check))
        .route(
            "/bridge-tasks",
            get(handlers::bridge_handler::list_inbound_tasks),
        )
        .route(
            "/bridge-results",
            get(handlers::bridge_handler::list_outbound_results),
        )
        .route("/bridge-ping", get(handlers::bridge_handler::ping_bridge))
        .route(
            "/extension.zip",
            get(handlers::extension_download_handler::download_extension),
        )
        .route(
            "/internal/dashboard-data-seed",
            post(handlers::internal_handler::seed_dashboard_data),
        )
        .route(
            "/provider-presets",
            get(handlers::integration_dispatch_handler::list_provider_presets),
        )
        .route(
            "/available-providers",
            get(handlers::provider_keys_handler::list_available_providers),
        )
        .route(
            "/industries",
            get(handlers::industry_handler::list_industries),
        )
        .merge(public_body_routes);

    // Combine: public + protected merged
    let api_v1 = Router::new().merge(public_routes).merge(protected_routes);

    Router::new()
        .route("/", get(health_check))
        .nest("/api/v1", api_v1)
        .with_state(state)
}

async fn health_check() -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "ok",
            "service": "workflowswift-api",
            "version": env!("CARGO_PKG_VERSION")
        })),
    )
}
