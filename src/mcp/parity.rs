//! WorkflowSwift MCP door — the agent-parity tools (kanban t_8df5d174).
//!
//! Twenty-four tools that close the "every UI action reachable by key" families on the card:
//! run a workflow, settle a waiting run step, leads / competitors / brand monitors / tickets /
//! automations CRUD, template import, rendition create, and prospecting. They sit beside the
//! twenty-five tools in [`super::tools`] and follow the same rules:
//!
//! 1. The tenant filter lives INSIDE every statement (`aid::text = any($n::text[])`), so a foreign
//!    id answers `not_found` — the same answer a missing id gets — and never a foreign row.
//! 2. No statement is built at run time. Every one is a fixed literal; there is not one `format!`.
//! 3. Archive is soft: `is_active = false` where the column exists (competitors, brand monitors,
//!    automations), else the replay-safe `archived_at = now()` pattern (leads, tickets — migration
//!    136 adds the column). Nothing hard-deletes.
//!
//! The ACTION tools (run workflow, decide step, run automation, import template, create rendition,
//! prospect search/enrich) do NOT re-implement business logic: each calls a `pub(crate)` core on the
//! REST handler it mirrors (`workflow_handler::run_workflow_as`, `instance_handler::decide_step_as`,
//! `automation_handler::run_automation_as`, `template_handler::import_template_as`,
//! `rendition_handler::create_rendition_as` / `create_stitch_group_as`,
//! `prospecting_handler::search_businesses_as` / `enrich_business_as`), so the door and the console
//! share one validation and one write path and can never drift apart.
//!
//! `prospect_search` / `enrich_lead` spend nothing and write nothing: they call the account's own
//! provider credential exactly as the console does, and refuse honestly (424-shaped `unavailable`)
//! when no provider key is connected — never an invented company.

use mcp_door::{Registry, Role, ToolError, ToolSpec};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers;
use crate::mcp::tools::{
    id_plus_schema, id_schema, list_schema, opt_i64, opt_json, opt_str, opt_uuid, prop, req_str,
    scope_id, scope_ids, write_schema,
};
use crate::AppState;

/// Map a handler's `AppError` to the door's `ToolError` vocabulary. A foreign or missing row is
/// `NotFound` (never `Forbidden`); a validation refusal keeps its sentence; an upstream refusal
/// (a provider not connected) is `Unavailable`; anything else is an audited internal.
fn map(e: AppError) -> ToolError {
    match e {
        AppError::NotFound(_) => ToolError::NotFound,
        AppError::Validation(m) | AppError::BadRequest(m) => ToolError::Invalid(m),
        AppError::Forbidden(m) => ToolError::Forbidden(m),
        AppError::UpstreamRefused(m) | AppError::Upstream(m) => ToolError::Unavailable(m),
        other => ToolError::Internal(other.to_string()),
    }
}

/// An optional float argument (`competitors.market_share` is float8).
fn opt_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

/// An optional string-array argument (competitors strengths/weaknesses, brand-monitor keywords).
fn opt_str_array(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// A customer may not close their own ticket (kanban t_d23f1330): the operator alone moves a
/// request to `resolved`/`closed`. The MCP key is a workspace credential, so the same rule the REST
/// `create`/`update` apply (`tickets_handler::refuse_customer_close`) is enforced here.
fn ticket_status_ok(status: Option<&str>) -> Result<(), ToolError> {
    if let Some(s) = status {
        if matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "resolved" | "closed"
        ) {
            return Err(ToolError::Forbidden(
                "only the team can mark a request resolved or closed; to withdraw it, archive it"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// The account's primary owner user, for attributing a rendition a machine key creates. The
/// resolver already proved the key's owning user is active, so an account with no active user is a
/// defect, not a normal state.
async fn owner_user_id(state: &AppState, aid: Uuid) -> Result<Uuid, ToolError> {
    let uid: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE aid = $1 AND is_active \
         ORDER BY (role IN ('owner','company_admin')) DESC, sort_order NULLS LAST, created_at ASC \
         LIMIT 1",
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ToolError::Internal(format!("rendition attribution: {e}")))?;
    uid.ok_or_else(|| {
        ToolError::Internal("the account has no active user to attribute the rendition".into())
    })
}

/// Register the twenty-four parity tools onto `reg`.
pub fn register(mut reg: Registry, state: AppState) -> Registry {
    // ── run a workflow ──────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_run_workflow",
            "Run ONE of this account's own workflows NOW, in-process, exactly as the console's Run \
             does: the run spends one credit and always leaves a `workflow_instances` row. Takes \
             `workflow_id` (required), plus optional `name`, `payload` (JSON) and `client_id`. \
             Returns the run status, instance_id, per-step outcomes, warnings and the remaining \
             balance. A foreign or missing workflow id answers not_found; nothing here bypasses the \
             credit charge.",
        )
        .min_role(Role::Workspace)
        .open_world()
        .schema(write_schema(
            json!({
                "workflow_id": prop("string", "The workflow uuid to run (required)."),
                "name": prop("string", "Optional run label (defaults to the workflow name)."),
                "payload": prop("object", "Optional input payload for the run's Data Card."),
                "client_id": prop("string", "Optional client uuid the run belongs to (defaults to the account's system client)."),
            }),
            &["workflow_id"],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let workflow_id = ctx.uuid_arg(&args, "workflow_id")?;
                let triggered_by = ctx.caller.subject.clone();
                handlers::workflow_handler::run_workflow_as(&state, aid, workflow_id, args, &triggered_by)
                    .await
                    .map_err(map)
            })
        },
    );

    // ── settle a waiting run step ───────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_decide_run_step",
            "Approve or reject ONE waiting step of this account's own run (a manual/approval gate, \
             or fast-forward a delay). Takes `instance_id`, `step_id` and `decision` \
             ('approve'|'reject'). The step must belong to an instance of the caller's account — a \
             foreign or missing id answers not_found. Does NOT bill; the run was charged once at \
             trigger time.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "instance_id": prop("string", "The workflow instance (run) uuid (required)."),
                "step_id": prop("string", "The instance step uuid (required)."),
                "decision": prop("string", "One of 'approve' or 'reject' (required)."),
            }),
            &["instance_id", "step_id", "decision"],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let instance_id = ctx.uuid_arg(&args, "instance_id")?;
                let step_id = ctx.uuid_arg(&args, "step_id")?;
                let actor = ctx.caller.subject.clone();
                handlers::instance_handler::decide_step_as(&state, aid, instance_id, step_id, args, &actor)
                    .await
                    .map_err(map)
            })
        },
    );

    // ── leads ───────────────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_list_leads",
            "List this account's leads (workflow inputs), newest first. Returns { count, items: [{ \
             id, name, email, phone, company, status, source, surface_id, workflow_id, \
             notes_excerpt (500 chars of untrusted text), created_at, updated_at }] }. Optional \
             `status` filter and `active_only` (true hides archived). Read-only; another account's \
             leads are never returned.",
        )
        .min_role(Role::ReadOnly)
        .schema(list_schema(json!({
            "status": { "type": "string", "description": "e.g. new, contacted, qualified, lost." },
            "active_only": { "type": "boolean", "description": "When true, only non-archived leads." }
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let limit = ctx.limit(args.get("limit"), 25, 100);
                let status = opt_str(&args, "status");
                let active_only = args
                    .get("active_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let row: Value = sqlx::query_scalar(
                    "select json_build_object('count', json_array_length(a.items), 'items', a.items) \
                     from ( \
                       select coalesce(json_agg(json_build_object( \
                           'id', l.id, 'name', l.name, 'email', l.email, 'phone', l.phone, \
                           'company', l.company, 'status', l.status, 'source', l.source, \
                           'surface_id', l.surface_id, 'workflow_id', l.workflow_id, \
                           'notes_excerpt', left(coalesce(l.notes, ''), 500), \
                           'created_at', l.created_at, 'updated_at', l.updated_at \
                         ) order by l.created_at desc), '[]'::json) as items \
                       from ( \
                         select * from leads \
                          where aid::text = any($2::text[]) \
                            and ($3::text is null or status = $3) \
                            and ($4::boolean is not true or archived_at is null) \
                          order by created_at desc limit $1 \
                       ) l \
                     ) a",
                )
                .bind(limit)
                .bind(scope_ids(&ctx))
                .bind(status)
                .bind(active_only)
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("list_leads: {e}")))?;
                Ok(row)
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_create_lead",
            "Create ONE lead in this account's own workspace. `name` is required; `email`, `phone`, \
             `company`, `status`, `source`, `surface_id`, `workflow_id` and `notes` are optional. \
             Returns { id, count }. The account is this credential's; another account is never \
             reachable.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "name": prop("string", "Lead name (required)."),
                "email": prop("string", "Optional contact email."),
                "phone": prop("string", "Optional phone number."),
                "company": prop("string", "Optional company."),
                "status": prop("string", "Optional status (defaults to 'new')."),
                "source": prop("string", "Optional source label."),
                "surface_id": prop("string", "Optional surface uuid."),
                "workflow_id": prop("string", "Optional workflow uuid this lead feeds."),
                "notes": prop("string", "Optional free-text notes."),
            }),
            &["name"],
        )),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let name = req_str(&args, "name")?;
                let aid = scope_id(&ctx)?;
                let id: Uuid = sqlx::query_scalar(
                    "insert into leads (id, aid, name, email, phone, company, status, source, \
                        surface_id, workflow_id, notes) \
                     values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                     returning id",
                )
                .bind(Uuid::new_v4())
                .bind(aid)
                .bind(name)
                .bind(opt_str(&args, "email"))
                .bind(opt_str(&args, "phone"))
                .bind(opt_str(&args, "company"))
                .bind(opt_str(&args, "status").unwrap_or_else(|| "new".into()))
                .bind(opt_str(&args, "source"))
                .bind(opt_uuid(&args, "surface_id")?)
                .bind(opt_uuid(&args, "workflow_id")?)
                .bind(opt_str(&args, "notes"))
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("create_lead: {e}")))?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_update_lead",
            "Update ONE of this account's OWN leads by id; every field is optional and omitted \
             fields keep their value. Returns { id, count }. A foreign or missing id answers \
             not_found.",
        )
        .min_role(Role::Workspace)
        .schema(id_plus_schema(json!({
            "name": prop("string", "Lead name."),
            "email": prop("string", "Contact email."),
            "phone": prop("string", "Phone number."),
            "company": prop("string", "Company."),
            "status": prop("string", "Status."),
            "source": prop("string", "Source label."),
            "surface_id": prop("string", "Surface uuid."),
            "workflow_id": prop("string", "Workflow uuid."),
            "notes": prop("string", "Free-text notes."),
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                let updated: Option<Uuid> = sqlx::query_scalar(
                    "update leads set \
                       name = coalesce($3, name), \
                       email = coalesce($4, email), \
                       phone = coalesce($5, phone), \
                       company = coalesce($6, company), \
                       status = coalesce($7, status), \
                       source = coalesce($8, source), \
                       surface_id = coalesce($9, surface_id), \
                       workflow_id = coalesce($10, workflow_id), \
                       notes = coalesce($11, notes), \
                       updated_at = now() \
                     where id = $1 and aid::text = any($2::text[]) \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .bind(opt_str(&args, "name"))
                .bind(opt_str(&args, "email"))
                .bind(opt_str(&args, "phone"))
                .bind(opt_str(&args, "company"))
                .bind(opt_str(&args, "status"))
                .bind(opt_str(&args, "source"))
                .bind(opt_uuid(&args, "surface_id")?)
                .bind(opt_uuid(&args, "workflow_id")?)
                .bind(opt_str(&args, "notes"))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("update_lead: {e}")))?;
                let id = updated.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_archive_lead",
            "Archive ONE of this account's OWN leads by id: sets archived_at = now() (soft — the \
             row is kept and stays readable). Returns { id, count }. A foreign or missing id \
             answers not_found. Nothing here hard-deletes.",
        )
        .min_role(Role::Workspace)
        .schema(id_schema()),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                let archived: Option<Uuid> = sqlx::query_scalar(
                    "update leads set archived_at = now() \
                     where id = $1 and aid::text = any($2::text[]) and archived_at is null \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("archive_lead: {e}")))?;
                let id = archived.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    // ── competitors ─────────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_list_competitors",
            "List this account's tracked competitors, name order. Returns { count, items: [{ id, \
             name, website, description (400-char excerpt), strengths, weaknesses, market_share, \
             is_active, last_checked_at, created_at }] }. Optional `active_only`. Read-only; \
             another account's competitors are never returned.",
        )
        .min_role(Role::ReadOnly)
        .schema(list_schema(json!({
            "active_only": { "type": "boolean", "description": "When true, only active competitors." }
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let limit = ctx.limit(args.get("limit"), 25, 100);
                let active_only = args
                    .get("active_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let row: Value = sqlx::query_scalar(
                    "select json_build_object('count', json_array_length(a.items), 'items', a.items) \
                     from ( \
                       select coalesce(json_agg(json_build_object( \
                           'id', c.id, 'name', c.name, 'website', c.website, \
                           'description', left(coalesce(c.description, ''), 400), \
                           'strengths', c.strengths, 'weaknesses', c.weaknesses, \
                           'market_share', c.market_share, \
                           'is_active', coalesce(c.is_active, true), \
                           'last_checked_at', c.last_checked_at, 'created_at', c.created_at \
                         ) order by c.name), '[]'::json) as items \
                       from ( \
                         select * from competitors \
                          where aid::text = any($2::text[]) \
                            and ($3::boolean is not true or coalesce(is_active, true)) \
                          order by name limit $1 \
                       ) c \
                     ) a",
                )
                .bind(limit)
                .bind(scope_ids(&ctx))
                .bind(active_only)
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("list_competitors: {e}")))?;
                Ok(row)
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_create_competitor",
            "Create ONE competitor this account tracks. `name` is required; `website`, \
             `description`, `strengths` (string array), `weaknesses` (string array) and \
             `market_share` (number) are optional. Returns { id, count }. The account is this \
             credential's.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "name": prop("string", "Competitor name (required)."),
                "website": prop("string", "Optional website URL."),
                "description": prop("string", "Optional description."),
                "strengths": prop("array", "Optional string array of strengths."),
                "weaknesses": prop("array", "Optional string array of weaknesses."),
                "market_share": prop("number", "Optional market share estimate."),
            }),
            &["name"],
        )),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let name = req_str(&args, "name")?;
                let aid = scope_id(&ctx)?;
                let id: Uuid = sqlx::query_scalar(
                    "insert into competitors (id, aid, name, website, description, strengths, \
                        weaknesses, market_share, is_active) \
                     values ($1, $2, $3, $4, $5, $6::text[], $7::text[], $8, true) \
                     returning id",
                )
                .bind(Uuid::new_v4())
                .bind(aid)
                .bind(name)
                .bind(opt_str(&args, "website"))
                .bind(opt_str(&args, "description"))
                .bind(opt_str_array(&args, "strengths"))
                .bind(opt_str_array(&args, "weaknesses"))
                .bind(opt_f64(&args, "market_share"))
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("create_competitor: {e}")))?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_update_competitor",
            "Update ONE of this account's OWN competitors by id; every field is optional and \
             omitted fields keep their value. Returns { id, count }. A foreign or missing id \
             answers not_found.",
        )
        .min_role(Role::Workspace)
        .schema(id_plus_schema(json!({
            "name": prop("string", "Competitor name."),
            "website": prop("string", "Website URL."),
            "description": prop("string", "Description."),
            "strengths": prop("array", "String array of strengths."),
            "weaknesses": prop("array", "String array of weaknesses."),
            "market_share": prop("number", "Market share estimate."),
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                let updated: Option<Uuid> = sqlx::query_scalar(
                    "update competitors set \
                       name = coalesce($3, name), \
                       website = coalesce($4, website), \
                       description = coalesce($5, description), \
                       strengths = coalesce($6::text[], strengths), \
                       weaknesses = coalesce($7::text[], weaknesses), \
                       market_share = coalesce($8, market_share), \
                       updated_at = now() \
                     where id = $1 and aid::text = any($2::text[]) \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .bind(opt_str(&args, "name"))
                .bind(opt_str(&args, "website"))
                .bind(opt_str(&args, "description"))
                .bind(opt_str_array(&args, "strengths"))
                .bind(opt_str_array(&args, "weaknesses"))
                .bind(opt_f64(&args, "market_share"))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("update_competitor: {e}")))?;
                let id = updated.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_archive_competitor",
            "Archive ONE of this account's OWN competitors by id: sets is_active = false (soft). \
             Returns { id, count }. A foreign or missing id answers not_found. Nothing here \
             hard-deletes.",
        )
        .min_role(Role::Workspace)
        .schema(id_schema()),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                let archived: Option<Uuid> = sqlx::query_scalar(
                    "update competitors set is_active = false, updated_at = now() \
                     where id = $1 and aid::text = any($2::text[]) \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("archive_competitor: {e}")))?;
                let id = archived.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    // ── brand monitors ──────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_list_brand_monitors",
            "List this account's brand monitors, brand name order. Returns { count, items: [{ id, \
             brand_name, keywords, platforms, is_active, last_scanned_at, created_at }] }. \
             Optional `active_only`. Read-only; another account's monitors are never returned.",
        )
        .min_role(Role::ReadOnly)
        .schema(list_schema(json!({
            "active_only": { "type": "boolean", "description": "When true, only active monitors." }
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let limit = ctx.limit(args.get("limit"), 25, 100);
                let active_only = args
                    .get("active_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let row: Value = sqlx::query_scalar(
                    "select json_build_object('count', json_array_length(a.items), 'items', a.items) \
                     from ( \
                       select coalesce(json_agg(json_build_object( \
                           'id', b.id, 'brand_name', b.brand_name, \
                           'keywords', b.keywords, 'platforms', b.platforms, \
                           'is_active', coalesce(b.is_active, true), \
                           'last_scanned_at', b.last_scanned_at, 'created_at', b.created_at \
                         ) order by b.brand_name), '[]'::json) as items \
                       from ( \
                         select * from brand_monitors \
                          where aid::text = any($2::text[]) \
                            and ($3::boolean is not true or coalesce(is_active, true)) \
                          order by brand_name limit $1 \
                       ) b \
                     ) a",
                )
                .bind(limit)
                .bind(scope_ids(&ctx))
                .bind(active_only)
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("list_brand_monitors: {e}")))?;
                Ok(row)
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_create_brand_monitor",
            "Create ONE brand monitor for this account. `brand_name` and a non-empty `platforms` \
             (string array) are required; `keywords` (string array) is optional. Returns { id, \
             count }. The account is this credential's.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "brand_name": prop("string", "The brand to monitor (required)."),
                "platforms": prop("array", "String array of platforms to scan (required, non-empty)."),
                "keywords": prop("array", "Optional string array of keywords."),
            }),
            &["brand_name", "platforms"],
        )),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let brand_name = req_str(&args, "brand_name")?;
                let platforms = opt_str_array(&args, "platforms").unwrap_or_default();
                if platforms.is_empty() {
                    return Err(ToolError::Invalid("`platforms` must name at least one platform".into()));
                }
                let aid = scope_id(&ctx)?;
                let id: Uuid = sqlx::query_scalar(
                    "insert into brand_monitors (id, aid, brand_name, keywords, platforms, is_active) \
                     values ($1, $2, $3, $4::text[], $5::text[], true) \
                     returning id",
                )
                .bind(Uuid::new_v4())
                .bind(aid)
                .bind(brand_name)
                .bind(opt_str_array(&args, "keywords"))
                .bind(platforms)
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("create_brand_monitor: {e}")))?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_archive_brand_monitor",
            "Archive ONE of this account's OWN brand monitors by id: sets is_active = false \
             (soft). Returns { id, count }. A foreign or missing id answers not_found. Nothing here \
             hard-deletes.",
        )
        .min_role(Role::Workspace)
        .schema(id_schema()),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                let archived: Option<Uuid> = sqlx::query_scalar(
                    "update brand_monitors set is_active = false, updated_at = now() \
                     where id = $1 and aid::text = any($2::text[]) \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("archive_brand_monitor: {e}")))?;
                let id = archived.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    // ── tickets ─────────────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_list_tickets",
            "List this account's support tickets, newest first. Returns { count, items: [{ id, \
             subject, description (400-char excerpt), status, priority, source, reply (400-char \
             excerpt), created_at, updated_at }] }. Optional `status` filter and `active_only` \
             (true hides archived). Read-only; another account's tickets are never returned.",
        )
        .min_role(Role::ReadOnly)
        .schema(list_schema(json!({
            "status": { "type": "string", "description": "e.g. open, pending, closed, resolved." },
            "active_only": { "type": "boolean", "description": "When true, only non-archived tickets." }
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let limit = ctx.limit(args.get("limit"), 25, 100);
                let status = opt_str(&args, "status");
                let active_only = args
                    .get("active_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let row: Value = sqlx::query_scalar(
                    "select json_build_object('count', json_array_length(a.items), 'items', a.items) \
                     from ( \
                       select coalesce(json_agg(json_build_object( \
                           'id', t.id, 'subject', t.subject, \
                           'description', left(coalesce(t.description, ''), 400), \
                           'status', t.status, 'priority', t.priority, 'source', t.source, \
                           'reply', left(coalesce(t.reply, ''), 400), \
                           'created_at', t.created_at, 'updated_at', t.updated_at \
                         ) order by t.created_at desc), '[]'::json) as items \
                       from ( \
                         select * from tickets \
                          where aid::text = any($2::text[]) \
                            and ($3::text is null or status = $3) \
                            and ($4::boolean is not true or archived_at is null) \
                          order by created_at desc limit $1 \
                       ) t \
                     ) a",
                )
                .bind(limit)
                .bind(scope_ids(&ctx))
                .bind(status)
                .bind(active_only)
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("list_tickets: {e}")))?;
                Ok(row)
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_create_ticket",
            "File ONE support ticket in this account's own workspace. `subject` is required; \
             `description`, `status`, `priority` and `source` are optional. A status of \
             'resolved'/'closed' is refused (that move is the team's alone — archive instead to \
             withdraw). Returns { id, count }.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "subject": prop("string", "Ticket subject (required)."),
                "description": prop("string", "Optional description."),
                "status": prop("string", "Optional status (defaults to 'open')."),
                "priority": prop("string", "Optional priority (defaults to 'medium')."),
                "source": prop("string", "Optional source (defaults to 'manual')."),
            }),
            &["subject"],
        )),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let subject = req_str(&args, "subject")?;
                ticket_status_ok(opt_str(&args, "status").as_deref())?;
                let aid = scope_id(&ctx)?;
                let id: Uuid = sqlx::query_scalar(
                    "insert into tickets (id, aid, subject, description, status, priority, source) \
                     values ($1, $2, $3, $4, $5, $6, $7) \
                     returning id",
                )
                .bind(Uuid::new_v4())
                .bind(aid)
                .bind(subject)
                .bind(opt_str(&args, "description").unwrap_or_default())
                .bind(opt_str(&args, "status").unwrap_or_else(|| "open".into()))
                .bind(opt_str(&args, "priority").unwrap_or_else(|| "medium".into()))
                .bind(opt_str(&args, "source").unwrap_or_else(|| "manual".into()))
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("create_ticket: {e}")))?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_update_ticket",
            "Update ONE of this account's OWN tickets by id; every field is optional and omitted \
             fields keep their value. A status of 'resolved'/'closed' is refused (the team's move \
             alone). Returns { id, count }. A foreign or missing id answers not_found.",
        )
        .min_role(Role::Workspace)
        .schema(id_plus_schema(json!({
            "subject": prop("string", "Ticket subject."),
            "description": prop("string", "Description."),
            "status": prop("string", "Status."),
            "priority": prop("string", "Priority."),
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                ticket_status_ok(opt_str(&args, "status").as_deref())?;
                let updated: Option<Uuid> = sqlx::query_scalar(
                    "update tickets set \
                       subject = coalesce($3, subject), \
                       description = coalesce($4, description), \
                       status = coalesce($5, status), \
                       priority = coalesce($6, priority), \
                       updated_at = now() \
                     where id = $1 and aid::text = any($2::text[]) \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .bind(opt_str(&args, "subject"))
                .bind(opt_str(&args, "description"))
                .bind(opt_str(&args, "status"))
                .bind(opt_str(&args, "priority"))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("update_ticket: {e}")))?;
                let id = updated.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_archive_ticket",
            "Archive (withdraw) ONE of this account's OWN tickets by id: sets archived_at = now() \
             (soft — the row and its reply stay readable). Returns { id, count }. A foreign or \
             missing id answers not_found. Nothing here hard-deletes.",
        )
        .min_role(Role::Workspace)
        .schema(id_schema()),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let id = ctx.uuid_arg(&args, "id")?;
                let archived: Option<Uuid> = sqlx::query_scalar(
                    "update tickets set archived_at = now() \
                     where id = $1 and aid::text = any($2::text[]) and archived_at is null \
                     returning id",
                )
                .bind(id)
                .bind(scope_ids(&ctx))
                .fetch_optional(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("archive_ticket: {e}")))?;
                let id = archived.ok_or(ToolError::NotFound)?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    // ── automations ─────────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_list_automations",
            "List this account's automations, name order. Returns { count, items: [{ id, name, \
             description (400-char excerpt), trigger_type, action_type, is_active, created_at, \
             updated_at }] }. The trigger/action CONFIG is not returned (it can carry webhook \
             URLs). Optional `active_only`. Read-only; another account's automations are never \
             returned.",
        )
        .min_role(Role::ReadOnly)
        .schema(list_schema(json!({
            "active_only": { "type": "boolean", "description": "When true, only active automations." }
        }))),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let limit = ctx.limit(args.get("limit"), 25, 100);
                let active_only = args
                    .get("active_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let row: Value = sqlx::query_scalar(
                    "select json_build_object('count', json_array_length(a.items), 'items', a.items) \
                     from ( \
                       select coalesce(json_agg(json_build_object( \
                           'id', am.id, 'name', am.name, \
                           'description', left(coalesce(am.description, ''), 400), \
                           'trigger_type', am.trigger_type, 'action_type', am.action_type, \
                           'is_active', coalesce(am.is_active, true), \
                           'created_at', am.created_at, 'updated_at', am.updated_at \
                         ) order by am.name), '[]'::json) as items \
                       from ( \
                         select * from automations \
                          where aid::text = any($2::text[]) \
                            and ($3::boolean is not true or coalesce(is_active, true)) \
                          order by name limit $1 \
                       ) am \
                     ) a",
                )
                .bind(limit)
                .bind(scope_ids(&ctx))
                .bind(active_only)
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("list_automations: {e}")))?;
                Ok(row)
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_create_automation",
            "Create ONE automation in this account's own workspace. `name` is required; \
             `description`, `trigger_type` (default 'manual'), `trigger_config` (JSON), \
             `action_type` (default 'webhook') and `action_config` (JSON) are optional. Returns \
             { id, count }. The account is this credential's.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "name": prop("string", "Automation name (required)."),
                "description": prop("string", "Optional description."),
                "trigger_type": prop("string", "Optional trigger type (default 'manual')."),
                "trigger_config": prop("object", "Optional trigger configuration JSON."),
                "action_type": prop("string", "Optional action type (default 'webhook')."),
                "action_config": prop("object", "Optional action configuration JSON."),
            }),
            &["name"],
        )),
        move |ctx, args| {
            let pool = s.db.clone();
            Box::pin(async move {
                let name = req_str(&args, "name")?;
                let aid = scope_id(&ctx)?;
                let id: Uuid = sqlx::query_scalar(
                    "insert into automations (id, aid, name, description, trigger_type, \
                        trigger_config, action_type, action_config, is_active) \
                     values ($1, $2, $3, $4, $5, $6, $7, $8, true) \
                     returning id",
                )
                .bind(Uuid::new_v4())
                .bind(aid)
                .bind(name)
                .bind(opt_str(&args, "description"))
                .bind(opt_str(&args, "trigger_type").unwrap_or_else(|| "manual".into()))
                .bind(opt_json(&args, "trigger_config").unwrap_or_else(|| json!({})))
                .bind(opt_str(&args, "action_type").unwrap_or_else(|| "webhook".into()))
                .bind(opt_json(&args, "action_config").unwrap_or_else(|| json!({})))
                .fetch_one(&pool)
                .await
                .map_err(|e| ToolError::Internal(format!("create_automation: {e}")))?;
                Ok(json!({ "id": id.to_string(), "count": 1 }))
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_run_automation",
            "Trigger ONE of this account's OWN automations by id, exactly as the console's Run \
             does: writes an `automation_runs` row ('running') with the trigger data. `id` is \
             required; `trigger_data` (JSON) is optional. Returns { run_id, status }. A foreign or \
             missing id answers not_found and can never trigger another tenant's automation.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "id": prop("string", "The automation uuid to run (required)."),
                "trigger_data": prop("object", "Optional trigger payload JSON."),
            }),
            &["id"],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let id = ctx.uuid_arg(&args, "id")?;
                let trigger_data = opt_json(&args, "trigger_data").unwrap_or_else(|| json!({}));
                handlers::automation_handler::run_automation_as(&state, aid, id, trigger_data)
                    .await
                    .map_err(map)
            })
        },
    );

    // ── template import ─────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_import_template",
            "Import ONE workflow template for this account (account-private, is_public=false), \
             exactly as the console's template import does: `name` and `category` are required, \
             `description`, `tags` (JSON), `surface_id` and `steps` (array of { step_type, name, \
             description, sort_order, config }) are optional. The same step validation the console \
             enforces applies (every step must be executable, step 1 must be a Data Card). Returns \
             { template, imported: true }. The account is this credential's.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "name": prop("string", "Template name (required)."),
                "category": prop("string", "Category label (required)."),
                "description": prop("string", "Optional description."),
                "tags": prop("array", "Optional JSON array of tag names."),
                "surface_id": prop("string", "Optional surface uuid."),
                "steps": prop("array", "Optional array of template steps."),
            }),
            &["name", "category"],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let steps_raw = args
                    .get("steps")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut steps = Vec::with_capacity(steps_raw.len());
                for raw in &steps_raw {
                    steps.push(crate::models::template::TemplateStep {
                        step_type: req_str(raw, "step_type")?,
                        name: req_str(raw, "name")?,
                        description: opt_str(raw, "description"),
                        sort_order: opt_i64(raw, "sort_order").map(|v| v as i32).unwrap_or(0),
                        config: opt_json(raw, "config"),
                    });
                }
                let req = crate::models::template::CreateTemplateRequest {
                    name: req_str(&args, "name")?,
                    description: opt_str(&args, "description"),
                    category: Some(req_str(&args, "category")?),
                    category_id: opt_uuid(&args, "category_id")?,
                    tags: opt_json(&args, "tags"),
                    surface_id: opt_uuid(&args, "surface_id")?,
                    steps,
                };
                handlers::template_handler::import_template_as(&state, aid, req)
                    .await
                    .map_err(map)
            })
        },
    );

    // ── rendition ───────────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::write(
            "workflowswift_create_rendition",
            "Create ONE rendition record for this account, exactly as the console's rendition \
             create does — `kind` 'rendition' (default) writes a media rendition (requires \
             `provider`, `provider_asset_id`, `provider_asset_url`, `asset_type`), `kind` 'stitch' \
             groups the caller's own child renditions (requires `child_ids`). Returns the rendition \
             row. The account is this credential's; there is no target id to spoof.",
        )
        .min_role(Role::Workspace)
        .schema(write_schema(
            json!({
                "kind": prop("string", "One of 'rendition' (default) or 'stitch'."),
                "provider": prop("string", "Provider key (required for kind=rendition)."),
                "provider_asset_id": prop("string", "Provider asset id (required for kind=rendition)."),
                "provider_asset_url": prop("string", "Provider asset URL (required for kind=rendition)."),
                "asset_type": prop("string", "Asset type (required for kind=rendition)."),
                "step_name": prop("string", "Optional step name."),
                "preview_url": prop("string", "Optional preview URL."),
                "thumbnail_url": prop("string", "Optional thumbnail URL."),
                "workflow_id": prop("string", "Optional workflow uuid."),
                "instance_id": prop("string", "Optional instance uuid."),
                "sort_order": prop("integer", "Optional sort position."),
                "retention_days": prop("integer", "Optional retention days (default 90)."),
                "metadata": prop("object", "Optional metadata JSON."),
                "name": prop("string", "Optional stitch group name (kind=stitch)."),
                "child_ids": prop("array", "String array of child rendition uuids (required for kind=stitch)."),
            }),
            &[],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let user_id = owner_user_id(&state, aid).await?;
                let kind = opt_str(&args, "kind").unwrap_or_else(|| "rendition".into());
                match kind.as_str() {
                    "stitch" => {
                        handlers::rendition_handler::create_stitch_group_as(&state, aid, user_id, args)
                            .await
                            .map_err(map)
                    }
                    _ => {
                        let req = crate::models::rendition::CreateRenditionRequest {
                            workflow_id: opt_uuid(&args, "workflow_id")?,
                            instance_id: opt_uuid(&args, "instance_id")?,
                            step_name: opt_str(&args, "step_name"),
                            provider: req_str(&args, "provider")?,
                            provider_asset_id: req_str(&args, "provider_asset_id")?,
                            provider_asset_url: req_str(&args, "provider_asset_url")?,
                            preview_url: opt_str(&args, "preview_url"),
                            thumbnail_url: opt_str(&args, "thumbnail_url"),
                            asset_type: req_str(&args, "asset_type")?,
                            sort_order: opt_i64(&args, "sort_order").map(|v| v as i32),
                            parent_rendition_id: opt_uuid(&args, "parent_rendition_id")?,
                            retention_days: opt_i64(&args, "retention_days").map(|v| v as i32),
                            metadata: opt_json(&args, "metadata"),
                        };
                        handlers::rendition_handler::create_rendition_as(&state, aid, user_id, req)
                            .await
                            .map_err(map)
                    }
                }
            })
        },
    );

    // ── prospecting ─────────────────────────────────────────────────────────────────────────────
    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_prospect_search",
            "Search businesses through the account's own connected search provider, exactly as the \
             console's Prospecting search does. `query` is required; `limit` (1-20, default 10) is \
             optional. Returns real provider results only — when the account has no provider key \
             connected the call refuses honestly ('unavailable') instead of inventing companies. \
             Read-only.",
        )
        .min_role(Role::ReadOnly)
        .open_world()
        .schema(write_schema(
            json!({
                "query": prop("string", "Business search query (required)."),
                "limit": prop("integer", "Max results 1-20 (default 10)."),
            }),
            &["query"],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let query = req_str(&args, "query")?;
                let limit = opt_i64(&args, "limit").unwrap_or(10).clamp(1, 20);
                handlers::prospecting_handler::search_businesses_as(&state, aid, &query, limit)
                    .await
                    .map_err(map)
            })
        },
    );

    let s = state.clone();
    reg = reg.with_fn(
        ToolSpec::read(
            "workflowswift_enrich_lead",
            "Enrich ONE business with the account's own connected search provider, exactly as the \
             console's Enrich does. `business_name` is required; `website` is optional. Returns \
             only what the provider really said — never an invented email, phone or insight. With \
             no provider key connected the call refuses honestly ('unavailable'); with no provider \
             match it answers not_found. Read-only.",
        )
        .min_role(Role::ReadOnly)
        .open_world()
        .schema(write_schema(
            json!({
                "business_name": prop("string", "The business to enrich (required)."),
                "website": prop("string", "Optional website, used when the provider has none."),
            }),
            &["business_name"],
        )),
        move |ctx, args| {
            let state = s.clone();
            Box::pin(async move {
                let aid = scope_id(&ctx)?;
                let business_name = req_str(&args, "business_name")?;
                let website = opt_str(&args, "website");
                handlers::prospecting_handler::enrich_business_as(
                    &state,
                    aid,
                    &business_name,
                    website,
                )
                .await
                .map_err(map)
            })
        },
    );

    reg
}

/// Every new tool this module registers — the parity acceptance harness asserts exactly this set is
/// present beside the twenty-five original tools.
pub const PARITY_TOOLS: &[&str] = &[
    "workflowswift_run_workflow",
    "workflowswift_decide_run_step",
    "workflowswift_list_leads",
    "workflowswift_create_lead",
    "workflowswift_update_lead",
    "workflowswift_archive_lead",
    "workflowswift_list_competitors",
    "workflowswift_create_competitor",
    "workflowswift_update_competitor",
    "workflowswift_archive_competitor",
    "workflowswift_list_brand_monitors",
    "workflowswift_create_brand_monitor",
    "workflowswift_archive_brand_monitor",
    "workflowswift_list_tickets",
    "workflowswift_create_ticket",
    "workflowswift_update_ticket",
    "workflowswift_archive_ticket",
    "workflowswift_list_automations",
    "workflowswift_create_automation",
    "workflowswift_run_automation",
    "workflowswift_import_template",
    "workflowswift_create_rendition",
    "workflowswift_prospect_search",
    "workflowswift_enrich_lead",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A lazy pool plus a minimal state: enough for `register` to build the registry. No tool here
    /// touches the database during registration, so the connection string is never used.
    fn dummy_state() -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:***@127.0.0.1:1/nothing")
            .expect("lazy pool");
        let config = crate::config::AppConfig {
            host: String::new(),
            port: 0,
            database_url: String::new(),
            jwt_secret: String::new(),
            jwt_access_expiry: 0,
            jwt_refresh_expiry: 0,
            db_min_connections: 0,
            db_max_connections: 0,
            auth_in_flight_cap: 8,
            body_read_deadline_secs: 30,
            stripe_signature_tolerance_secs: 300,
            internal_sync_key: String::new(),
            n8n_url: String::new(),
            n8n_webhook_url: String::new(),
            n8n_api_key: String::new(),
            callback_base_url: String::new(),
            funnelswift_url: String::new(),
            coreswift_url: String::new(),
            paypal_webhook_id: String::new(),
        };
        AppState {
            db: pool.clone(),
            pool: pool.clone(),
            config,
            rate_limiters: crate::rate_limit::RateLimiters::new(30, 60),
            pre_auth_limiters: crate::rate_limit::RateLimiters::new(30, 60),
            auth_in_flight: crate::rate_limit::AuthInFlight::new(8),
            provider_key_cache: crate::rate_limit::ProviderKeyCache::new(300),
            login_throttle: crate::login_throttle::LoginThrottle::new(),
        }
    }

    #[tokio::test]
    async fn the_parity_registry_is_exactly_twenty_four_tools() {
        let reg = register(Registry::new(), dummy_state());
        let mut names = reg.names();
        names.sort();
        let mut expected: Vec<&str> = PARITY_TOOLS.to_vec();
        expected.sort();
        assert_eq!(
            names, expected,
            "the parity registry must be exactly these 24 tools"
        );
        assert!(
            reg.validate().is_empty(),
            "registry must validate at startup"
        );

        // The read-only tools stay reachable by a read-only key.
        let reads = [
            "workflowswift_list_leads",
            "workflowswift_list_competitors",
            "workflowswift_list_brand_monitors",
            "workflowswift_list_tickets",
            "workflowswift_list_automations",
            "workflowswift_prospect_search",
            "workflowswift_enrich_lead",
        ];
        let writes: Vec<&str> = PARITY_TOOLS
            .iter()
            .copied()
            .filter(|t| !reads.contains(t))
            .collect();
        for name in &reads {
            let tool = reg.get(name).expect("registered");
            let spec = tool.spec();
            assert!(spec.annotations.read_only, "{name} must be read-only");
            assert!(
                !spec.annotations.destructive,
                "{name} must not be destructive"
            );
            assert_eq!(
                spec.min_role,
                Role::ReadOnly,
                "{name} must allow a read-only key"
            );
        }
        for name in &writes {
            let tool = reg.get(name).expect("registered");
            let spec = tool.spec();
            assert!(!spec.annotations.read_only, "{name} must not be read-only");
            assert!(
                !spec.annotations.destructive,
                "{name} archives or writes reversibly"
            );
            assert_eq!(
                spec.min_role,
                Role::Workspace,
                "{name} must require a workspace key"
            );
            assert_eq!(spec.input_schema["type"], "object");
            assert!(
                spec.description.len() > 20,
                "{name} needs a real description"
            );
        }
    }
}
