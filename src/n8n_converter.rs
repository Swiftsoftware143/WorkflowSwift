//! WorkflowSwift → n8n workflow JSON converter.
//!
//! Takes a WorkflowSwift workflow (list of step types + configs)
//! and generates a valid n8n workflow JSON that can be imported
//! via `n8n import:workflow --input=file.json`.
//!
//! Architecture:
//!   WorkflowSwift (UI) → this converter → n8n import → n8n webhook trigger
//!
//! Each WorkflowSwift step_type maps to one or more n8n nodes:
//!   - "ai-action"    → OpenClaw HTTP Request node
//!   - "http-request" → n8n HTTP Request node
//!   - "data-card"    → dashboard push node
//!   - "export"       → Google Sheets / SendGrid / CSV
//!   - "notify"       → Email (n8n emailSend) / generic webhook
//!   - "delay"        → n8n Wait node
//!   - "fork"         → n8n Switch node (parallel branches)
//!   - "action"       → Generic API call
//!   - "transform"    → n8n Code / Set node
//!   - "openclaw"     → OpenClaw reasoning step
//!
//! Callbacks into THIS app exist only for routes the app actually serves — `credits/balance`,
//! `credits/deduct`, `dashboard/push-widget-data` and `renditions` (kanban t_642b6894). Every
//! other arm that used to emit one is RETIRED: the step keeps its place in the graph as a
//! `passthrough_node` whose notes name the capability the app does not have. The live census that
//! proves the remaining set is served lives in `/opt/swift/audits/t_642b6894/`, and
//! `app_callback_census_is_generated_from_the_converter` regenerates its input from this file.

use serde_json::{json, Value};
use uuid::Uuid;

/// The generated n8n workflow document.
pub struct N8nWorkflow {
    pub name: String,
    pub nodes: Vec<Value>,
    pub connections: Value,
    pub settings: Value,
    pub webhook_path: String,
}

/// Build an n8n workflow JSON from WorkflowSwift steps.
/// `steps` — a list of JSON objects, each with at minimum:
///   { "step_type": "...", "name": "...", "config": { ... } }
/// `aid` is used for webhook path namespacing.
/// `workflow_id` is the UUID of the WorkflowSwift workflow.
/// `workflow_name` is used as the n8n workflow title.
/// `callback_base_url` is the base URL for callbacks (e.g. "http://workflowswift:8085").
/// Build a callback URL into this crate's OWN router.
///
/// The entire API is mounted under `/api/v1` (src/routes.rs) while `CALLBACK_BASE_URL` is the
/// bare origin (`http://workflowswift:8085`). Appending `/api/...` by hand here produced
/// `.../api/credits/balance` and `.../api/dashboard/push-widget-data`, which 404 on every
/// generated workflow (kanban t_d81efc3c). Every callback URL in this file goes through this one
/// function, so the prefix can only ever be added exactly once — whatever `CALLBACK_BASE_URL`
/// is set to.
fn callback_url(base: &str, path: &str) -> String {
    format!(
        "{}/api/v1/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// The bearer every generated callback node presents to this app.
///
/// It is the CALLER's own token, taken off the **Webhook trigger item** — not `$json` and not
/// an n8n credential. Two measured reasons (kanban t_eca73c55):
///
///  * `$json.headers...` only exists on the FIRST callback node. Every later node's input is
///    the previous callback's RESPONSE (`{"balance":18,…}`), which has no `.headers`, so the
///    expression threw on every node after "Auth & Credit".
///  * `authentication: genericCredentialType` + `genericAuthType: httpHeaderAuth` demands
///    `node.credentials.httpHeaderAuth.id`, and the converter has no credential to attach, so
///    n8n refused the node at run time ("Credentials not found") and the whole graph executed
///    nothing. The node builds its own header, so it needs no credential: `authentication: none`.
///
/// One constant, substituted into all 18 callback nodes and re-asserted by
/// `harden_callback_nodes` (the choke point) plus a unit test, so a new arm cannot drift back
/// to a credential n8n will refuse or to a `$json.headers` read that only works once.
pub const CALLBACK_AUTH_EXPR: &str =
    "=Bearer {{ $('Webhook').first().json.headers.authorization.split(' ')[1] }}";

/// n8n Filter/IF (V2) operator pairs this install actually accepts.
///
/// Read out of the container's own runtime rather than guessed:
/// `n8n-workflow/dist/cjs/node-parameters/filter-parameter.js` switches on exactly
/// number = empty|notEmpty|equals|notEquals|gt|lt|gte|lte,
/// string = empty|notEmpty|equals|notEquals|contains|notContains|startsWith|notStartsWith|
/// endsWith|notEndsWith|regex|notRegex,
/// boolean = empty|notEmpty|true|false|equals|notEquals,
/// dateTime = empty|notEmpty|equals|notEquals|after|before|afterOrEquals|beforeOrEquals.
/// `largerEqual` is NOT among them (it exists only in n8n's V1 filter) — that is why
/// "Balance OK?" never ran: `Unknown filter parameter operator "number:largerEqual"`.
fn filter_operator(kind: &str, operation: &str) -> Value {
    json!({ "type": kind, "operation": operation })
}

/// A step config's field/path -> an n8n expression over the incoming item.
///
/// Accepts what a tenant console actually stores: a bare name (`balance`), a dotted path
/// (`$json.balance`) or a ready expression (`={{ $json.balance }}`).
fn field_expression(field: &str) -> String {
    let f = field.trim();
    if f.is_empty() {
        return "={{ $json }}".to_string();
    }
    if f.starts_with("={{") || f.starts_with("{{") {
        let inner = f.trim_start_matches("={{").trim_start_matches("{{").trim();
        let inner = inner.trim_end_matches("}}").trim();
        return format!("={{{{ {} }}}}", inner);
    }
    if let Some(rest) = f.strip_prefix('$') {
        // `$json.x` / `$json["x"]` / `$now`: already a valid n8n expression body.
        return format!("={{{{ ${} }}}}", rest);
    }
    let escaped = f.replace('\\', "\\\\").replace('"', "\\\"");
    format!("={{{{ $json[\"{}\"] }}}}", escaped)
}

/// A user `condition` step's (leftValue, rightValue, operator).
///
/// The old arm emitted `{"string": bool, "number": bool, "boolean": bool}` — not an operator
/// at all: n8n's filter needs `{type, operation}`, so every generated Condition node was
/// invalid and the run died there. The left side is coerced to the operator's own type
/// because these nodes carry `typeValidation: "strict"`, which compares TYPES, not just values.
fn condition_operands(field: &str, operator: &str, value: &Value) -> (String, Value, Value) {
    let field = field.trim();
    let bare = field_expression(field);
    let inner = bare
        .trim_start_matches("={{")
        .trim_end_matches("}}")
        .trim()
        .to_string();
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    match operator.trim() {
        "contains" => (
            format!("={{{{ String({} ?? '') }}}}", inner),
            json!(text),
            filter_operator("string", "contains"),
        ),
        "startsWith" => (
            format!("={{{{ String({} ?? '') }}}}", inner),
            json!(text),
            filter_operator("string", "startsWith"),
        ),
        "endsWith" => (
            format!("={{{{ String({} ?? '') }}}}", inner),
            json!(text),
            filter_operator("string", "endsWith"),
        ),
        "larger" | "largerEqual" | "greater" | "greaterEqual" | "gte" | "gt" => {
            let n = text.parse::<f64>().unwrap_or(0.0);
            let op = if matches!(operator.trim(), "gt" | "larger" | "greater") {
                "gt"
            } else {
                "gte"
            };
            (
                format!("={{{{ Number({} ?? 0) }}}}", inner),
                json!(n),
                filter_operator("number", op),
            )
        }
        "smaller" | "smallerEqual" | "less" | "lessEqual" | "lte" | "lt" => {
            let n = text.parse::<f64>().unwrap_or(0.0);
            let op = if operator.trim() == "lt" || operator.trim() == "smaller" {
                "lt"
            } else {
                "lte"
            };
            (
                format!("={{{{ Number({} ?? 0) }}}}", inner),
                json!(n),
                filter_operator("number", op),
            )
        }
        "isTrue" => (
            format!("={{{{ Boolean({}) }}}}", inner),
            json!(true),
            filter_operator("boolean", "true"),
        ),
        "isFalse" => (
            format!("={{{{ Boolean({}) }}}}", inner),
            json!(false),
            filter_operator("boolean", "false"),
        ),
        "notEquals" => (
            format!("={{{{ String({} ?? '') }}}}", inner),
            json!(text),
            filter_operator("string", "notEquals"),
        ),
        // Default arm is `equals`, and it is the only one that has to guess between a string
        // and a number: a numeric-looking rightValue compares as a number, anything else as a
        // string. Both forms keep the left side the same type as the right.
        _ => match text.parse::<f64>() {
            Ok(n) => (
                format!("={{{{ Number({} ?? 0) }}}}", inner),
                json!(n),
                filter_operator("number", "equals"),
            ),
            Err(_) => (
                format!("={{{{ String({} ?? '') }}}}", inner),
                json!(text),
                filter_operator("string", "equals"),
            ),
        },
    }
}

/// Force the auth contract onto every node that calls THIS app back.
///
/// The single choke point that makes the fix structural rather than 18 hand edits: whatever a
/// future arm writes, a node whose URL sits under this app's own `/api/v1/` prefix ends up
/// with `authentication: "none"` and the caller's bearer, and explicit `onError` so a failing
/// callback stops the run and is retained as a failed execution instead of disappearing.
///
/// The URL-prefix test is deliberately exact (the app's own configured origin) — a tenant's
/// own URL must never receive this app's bearer.
fn harden_callback_nodes(nodes: &mut [Value], callback_base_url: &str) {
    let prefix = format!("{}/api/v1/", callback_base_url.trim_end_matches('/'));
    for node in nodes.iter_mut() {
        if node.get("type").and_then(|t| t.as_str()) != Some("n8n-nodes-base.httpRequest") {
            continue;
        }
        let is_callback = node
            .get("parameters")
            .and_then(|p| p.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| u.starts_with(&prefix))
            .unwrap_or(false);
        if !is_callback {
            continue;
        }
        if let Some(obj) = node.as_object_mut() {
            obj.insert("onError".to_string(), json!("stopWorkflow"));
        }
        let Some(params) = node.get_mut("parameters").and_then(|p| p.as_object_mut()) else {
            continue;
        };
        params.remove("genericAuthType");
        params.insert("authentication".to_string(), json!("none"));
        params.insert("sendHeaders".to_string(), json!(true));
        let mut headers: Vec<Value> = vec![json!({
            "name": "Authorization",
            "value": CALLBACK_AUTH_EXPR
        })];
        let existing = params
            .get("headerParameters")
            .and_then(|h| h.get("parameters"))
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        for h in existing {
            let name = h.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("Authorization") {
                continue;
            }
            headers.push(h);
        }
        params.insert(
            "headerParameters".to_string(),
            json!({ "parameters": headers }),
        );
    }
}

pub fn convert_steps_to_n8n(
    steps: &[Value],
    aid: Uuid,
    workflow_id: Uuid,
    callback_base_url: &str,
) -> N8nWorkflow {
    let mut nodes: Vec<Value> = Vec::new();
    let mut connections_map = serde_json::Map::new();

    // Namespaced webhook path so account workflows don't collide
    let aid_short = &aid.to_string()[..8];
    let webhook_path = format!("wfs/{}/{:.8}", aid_short, workflow_id);

    // ===== Node 0: Webhook Trigger =====
    let webhook_id = format!("wfs_{:.8}_{:.8}", aid_short, workflow_id);
    let webhook_node = json!({
        "id": "webhook",
        "name": "Webhook",
        "type": "n8n-nodes-base.webhook",
        "typeVersion": 1,
        "position": [250, 300],
        "webhookId": webhook_id,
        "parameters": {
            "path": webhook_path,
            "options": {}
        }
    });
    nodes.push(webhook_node);

    // ===== Node 1: Auth & Credit Check =====
    let credit_node = json!({
        "id": "credit_check",
        "name": "Auth & Credit",
        "type": "n8n-nodes-base.httpRequest",
        "typeVersion": 4.2,
        "position": [450, 300],
        "parameters": {
            "method": "GET",
            "url": callback_url(callback_base_url, "credits/balance"),
            "authentication": "none",
            "sendHeaders": true,
            "headerParameters": {
                "parameters": [
                    {
                        "name": "Authorization",
                        "value": CALLBACK_AUTH_EXPR
                    }
                ]
            }
        }
    });
    nodes.push(credit_node);

    // ===== Node 2: Balance Check =====
    // The left side reads `/credits/balance`'s REAL response — `{"balance":20,"available":20,…}`,
    // a flat object. It used to read `$json["data"][0].balance`, a shape that endpoint has never
    // returned, so the expression resolved to `undefined` and the check was false for every
    // tenant that ever had credits. `Number(… ?? 0)` keeps the strict type check happy (the
    // operator is a number operator) whether the field is a number, a numeric string or absent.
    let balance_check_node = json!({
        "id": "balance_check",
        "name": "Balance OK?",
        "type": "n8n-nodes-base.if",
        "typeVersion": 2,
        "position": [650, 300],
        "parameters": {
            "conditions": {
                "combinator": "and",
                "options": {
                    "caseSensitive": true,
                    "typeValidation": "strict"
                },
                "conditions": [
                    {
                        "id": "has_balance",
                        "leftValue": "={{ Number($json.balance ?? 0) }}",
                        "rightValue": 1,
                        "operator": filter_operator("number", "gte")
                    }
                ]
            }
        }
    });
    nodes.push(balance_check_node);

    // Build connection chains
    let prev_node_id = "balance_check";
    let prev_output_index = 0; // 0 = success branch, 1 = failure branch

    // ===== Node 3: Deduct Credit =====
    let deduct_node = json!({
        "id": "deduct_credit",
        "name": "Deduct Credit",
        "type": "n8n-nodes-base.httpRequest",
        "typeVersion": 4.2,
        "position": [850, 200],
        "parameters": {
            "method": "POST",
            "url": callback_url(callback_base_url, "credits/deduct"),
            "authentication": "none",
            "sendHeaders": true,
            "headerParameters": {
                "parameters": [
                    {
                        "name": "Authorization",
                        "value": CALLBACK_AUTH_EXPR
                    }
                ]
            },
            "sendBody": true,
            "bodyParameters": {
                "parameters": [
                    {
                        "name": "workflow_id",
                        "value": workflow_id.to_string()
                    }
                ]
            }
        }
    });
    nodes.push(deduct_node);

    // ===== Convert user steps =====
    // Returns (all_step_nodes, step_output_ids), where step_output_ids[i] is the
    // ID of the last node produced by step i (the one to wire forward).
    let (step_nodes, step_output_ids) = convert_user_steps(
        steps,
        aid,
        workflow_id,
        callback_base_url,
        &mut connections_map,
    );

    // Wire Webhook → Credit Check
    let mut webhook_conn = serde_json::Map::new();
    webhook_conn.insert(
        "main".to_string(),
        json!([[{"node": "credit_check", "type": "main", "index": 0}]]),
    );
    connections_map.insert("webhook".to_string(), Value::Object(webhook_conn));

    // Wire Credit Check → Balance Check
    let mut credit_conn = serde_json::Map::new();
    credit_conn.insert(
        "main".to_string(),
        json!([[{"node": "balance_check", "type": "main", "index": 0}]]),
    );
    connections_map.insert("credit_check".to_string(), Value::Object(credit_conn));

    // Wire Balance Check success → Deduct Credit; failure → respond with error
    let mut balance_conn = serde_json::Map::new();
    balance_conn.insert(
        "main".to_string(),
        json!([
            [{"node": "deduct_credit", "type": "main", "index": 0}],
            [{"node": "respond", "type": "main", "index": 0}]
        ]),
    );
    connections_map.insert("balance_check".to_string(), Value::Object(balance_conn));

    // Wire Deduct → first user step's output node
    if let Some(first_output) = step_output_ids.first() {
        let mut deduct_conn = serde_json::Map::new();
        deduct_conn.insert(
            "main".to_string(),
            json!([[{"node": first_output, "type": "main", "index": 0}]]),
        );
        connections_map.insert("deduct_credit".to_string(), Value::Object(deduct_conn));
    }

    // Wire user steps in sequence using step_output_ids
    for i in 0..step_output_ids.len() {
        let current_id = &step_output_ids[i];

        if i + 1 < step_output_ids.len() {
            let next_id = &step_output_ids[i + 1];
            let mut conn = serde_json::Map::new();
            conn.insert(
                "main".to_string(),
                json!([[{"node": next_id, "type": "main", "index": 0}]]),
            );
            connections_map.insert(current_id.clone(), Value::Object(conn));
        } else {
            // Last user step → Respond node
            let mut conn = serde_json::Map::new();
            conn.insert(
                "main".to_string(),
                json!([[{"node": "respond", "type": "main", "index": 0}]]),
            );
            connections_map.insert(current_id.clone(), Value::Object(conn));
        }
    }

    // ===== Final node: Response =====
    let respond_node = json!({
        "id": "respond",
        "name": "Respond to Webhook",
        "type": "n8n-nodes-base.respondToWebhook",
        "typeVersion": 1,
        "position": [250 + (step_output_ids.len() as i32 + 1) * 200, 600],
        "parameters": {
            "respondWith": "json",
            "responseBody": "={{ $json }}"
        }
    });
    nodes.push(respond_node);

    // Collect all nodes from user steps
    nodes.extend(step_nodes);

    // Settings.
    // `callerPolicy` is an enum in n8n's public API spec and was previously
    // "workflowsWithSameOwner", which is NOT a member — n8n 2.34.6 answers
    // 400 request/body/settings/callerPolicy must be equal to one of the allowed
    // values: any, none, workflowsFromAList, workflowsFromSameOwner.
    // `workflowsFromSameOwner` is the valid spelling of the same intent.
    let settings = json!({
        "timezone": "America/New_York",
        "saveDataErrorExecution": "all",
        "saveDataSuccessExecution": "all",
        "saveManualExecutions": true,
        "callerPolicy": "workflowsFromSameOwner",
    });

    // n8n addresses connections by NODE NAME, not by the node's `id`. Its validator
    // answers `unknown_connection_source` / `unknown_connection_target` for ids, so the
    // graph built above (keyed by ids such as "credit_check") is rejected with 400.
    // Rewrite it to names, forcing the names to be unique first — n8n resolves a name to
    // exactly one node, and a user step may repeat a name or be called "Webhook".
    //
    // Before that: one pass over every finished node enforces the app-callback contract
    // (`harden_callback_nodes`) — no credential, the caller's own bearer, explicit onError —
    // for the whole graph, in ONE place, whatever arm produced the node.
    harden_callback_nodes(&mut nodes, callback_base_url);
    let (nodes, connections) = names_and_connections(nodes, connections_map);

    N8nWorkflow {
        name: format!("WFS {}", workflow_id),
        nodes,
        connections,
        settings,
        webhook_path: webhook_path.clone(),
    }
}

/// Rewrite the id-keyed connection graph into n8n's name-keyed one, de-duplicating node
/// names on the way, and give every connection target the `type`/`index` fields n8n's
/// workflow-structure validator requires. See `to_n8n_json` for the other half of what
/// this n8n build accepts.
fn names_and_connections(
    nodes: Vec<Value>,
    connections_map: serde_json::Map<String, Value>,
) -> (Vec<Value>, Value) {
    use std::collections::{HashMap, HashSet};

    let mut used: HashSet<String> = HashSet::new();
    let mut id_to_name: HashMap<String, String> = HashMap::new();
    let mut out_nodes: Vec<Value> = Vec::with_capacity(nodes.len());

    for mut node in nodes {
        let id = node
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let base = node
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_string();
        let mut name = base.clone();
        let mut n = 1;
        while used.contains(&name) {
            n += 1;
            name = format!("{} ({})", base, n);
        }
        used.insert(name.clone());
        if !id.is_empty() {
            id_to_name.insert(id, name.clone());
        }
        if let Some(obj) = node.as_object_mut() {
            obj.insert("name".to_string(), Value::String(name));
        }
        out_nodes.push(node);
    }

    let mut out_conns = serde_json::Map::new();
    for (src_id, conn) in connections_map.iter() {
        let src = id_to_name
            .get(src_id)
            .cloned()
            .unwrap_or_else(|| src_id.clone());
        let mut conn = conn.clone();
        if let Some(main) = conn.get_mut("main").and_then(|m| m.as_array_mut()) {
            for output in main.iter_mut() {
                if let Some(targets) = output.as_array_mut() {
                    for target in targets.iter_mut() {
                        if let Some(obj) = target.as_object_mut() {
                            let target_name = obj
                                .get("node")
                                .and_then(|v| v.as_str())
                                .and_then(|v| id_to_name.get(v))
                                .cloned();
                            if let Some(target_name) = target_name {
                                obj.insert("node".to_string(), Value::String(target_name));
                            }
                            obj.entry("type")
                                .or_insert_with(|| Value::String("main".to_string()));
                            obj.entry("index").or_insert_with(|| json!(0));
                        }
                    }
                }
            }
        }
        out_conns.insert(src, conn);
    }

    (out_nodes, Value::Object(out_conns))
}

fn get_node_id(node: &Value) -> String {
    node.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// A pass-through node for a step type this app does not execute through the n8n mirror.
///
/// The converter used to build a callback URL for every step arm, and 13 of the 17 declared
/// targets were not served by this app (kanban t_642b6894): `provider-keys/{provider}/
/// {generate,design,research}`, `instances/loop-check`, `research/{source}`, `enrich/{provider}`,
/// `analyze`, `notifications/{channel}`, `engine/*` and `bridge/commands/publish` answered 404
/// (or matched a path that accepts other methods only), so every run that reached one of them
/// died there and the tenant's workflow stopped. None of those routes has an implementation
/// anywhere in the app (measured: no handler, no writer, and every search/enrich/research handler
/// is a hardcoded mock), so the arm was a fabrication rather than a spelling.
///
/// The step keeps its place in the graph as a no-op whose `notes` NAMES what is missing — the
/// same disposition the app's own engine applies to a step type it cannot execute
/// (`src/execution.rs`, the `_` arm: warning, run continues).
fn passthrough_node(
    node_id: &str,
    node_name: &str,
    pos: (i32, i32),
    step_type: &str,
    why: &str,
) -> Value {
    json!({
        "id": node_id,
        "name": node_name,
        "type": "n8n-nodes-base.noOp",
        "typeVersion": 1,
        "position": [pos.0, pos.1],
        "parameters": {},
        "notes": format!("WorkflowSwift step type '{}': {}", step_type, why)
    })
}

fn convert_user_steps(
    steps: &[Value],
    aid: Uuid,
    workflow_id: Uuid,
    callback_base_url: &str,
    connections_map: &mut serde_json::Map<String, Value>,
) -> (Vec<Value>, Vec<String>) {
    let mut nodes: Vec<Value> = Vec::new();
    // Track the last (output) node ID for each step
    let mut step_output_ids: Vec<String> = Vec::new();
    let _aid_prefix = &aid.to_string()[..8];
    let _wf_short = &workflow_id.to_string()[..8];

    for (i, step) in steps.iter().enumerate() {
        let step_type = step
            .get("step_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let step_name = step.get("name").and_then(|v| v.as_str()).unwrap_or("Step");
        let config = step
            .get("config")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        let x_pos = 250 + (i as i32 + 1) * 200;
        let y_base = 300;
        let node_id = format!("step_{}", i);

        // Track whether this step produced a custom last-node ID (for multi-node steps)
        let mut step_last_node_id: Option<String> = None;

        match step_type {
            "http-request" | "action" => {
                let method = config
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET");
                let url = config.get("url").and_then(|v| v.as_str()).unwrap_or("");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": method,
                        "url": url,
                        "authentication": "none",
                        "sendHeaders": config.get("headers").is_some(),
                        "sendBody": method != "GET",
                    }
                });
                nodes.push(node);
            }

            "ai-action" | "openclaw" => {
                let prompt = config.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let model = config
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("deepseek/deepseek-chat");

                // OpenClaw HTTP node — POST to the gateway
                // The user provides their OpenClaw gateway URL in Integration Center
                let openclaw_url = config
                    .get("gateway_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("http://localhost:18792");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/chat/completions", openclaw_url.trim_end_matches('/')),
                        "authentication": "none",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "model", "value": model },
                                { "name": "messages", "value": json!([
                                    {"role": "user", "content": prompt}
                                ])},
                                { "name": "temperature", "value": 0.7 }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            "data-card" => {
                // Push data to the dashboard
                let metric_key = config
                    .get("metric_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // `value_expression` is the widget's value. The default used to be
                // `={{ $json["data"] }}` — a path no callback response carries (the node's input
                // is the previous callback's body, e.g. `/credits/deduct`'s
                // `{"balance":18,"deducted":2,…}`). n8n drops a body parameter whose expression
                // resolves to `undefined`, and the app answers 400 "value required" — measured on
                // the live mirror (kanban t_eca73c55), and the tenant console never sets
                // `value_expression` at all (its step modal sends widget_name + metric_key only),
                // so every real workflow's Data Card push failed. The `report` arm below already
                // uses the incoming item; the response card now matches it, and stays defined.
                let value_expr = config
                    .get("value_expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("={{ $json }}");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": callback_url(callback_base_url, "dashboard/push-widget-data"),
                        "authentication": "none",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": CALLBACK_AUTH_EXPR },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "metric_key", "value": metric_key },
                                { "name": "value", "value": value_expr }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            "notify" => {
                let channel = config
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("email");
                let recipient = config
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let subject = config
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("WorkflowSwift Notification");
                let message = config.get("message").and_then(|v| v.as_str()).unwrap_or("");

                match channel {
                    "email" => {
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.emailSend",
                            "typeVersion": 1,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "fromEmail": "swiftsoftware143@yahoo.com",
                                "toEmail": recipient,
                                "subject": subject,
                                "text": message,
                                "options": {}
                            }
                        });
                        nodes.push(node);
                    }
                    // RETIRED callback (kanban t_642b6894): `slack` and `telegram` are not
                    // offered by the tenant console's Notify step (its Channel select is
                    // email | webhook | sms, www-app/index.html) and WorkflowSwift serves no
                    // slack/telegram sender, so the arm posted a route that never existed and
                    // the run died there.
                    "slack" | "telegram" => {
                        nodes.push(passthrough_node(
                            &node_id,
                            step_name,
                            (x_pos, y_base),
                            step_type,
                            &format!("channel '{}' has no sender in WorkflowSwift (the console offers email | webhook | sms)", channel),
                        ));
                    }
                    _ => {
                        // Generic webhook notification
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.webhook",
                            "typeVersion": 1,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "method": "POST",
                                "url": recipient,
                                "sendBody": true,
                                "bodyParameters": {
                                    "parameters": [
                                        { "name": "message", "value": message }
                                    ]
                                }
                            }
                        });
                        nodes.push(node);
                    }
                }
            }

            "export" => {
                let destination = config
                    .get("destination")
                    .and_then(|v| v.as_str())
                    .unwrap_or("http");
                match destination {
                    "google_sheets" | "sheets" => {
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.googleSheets",
                            "typeVersion": 4,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "operation": "append",
                                "documentId": config.get("sheet_id").and_then(|v| v.as_str()).unwrap_or(""),
                                "sheetName": config.get("sheet_name").and_then(|v| v.as_str()).unwrap_or("Sheet1"),
                                "columns": {
                                    "mappingMode": "defineBelow",
                                    "value": "={{ $json }}"
                                },
                                "options": {}
                            }
                        });
                        nodes.push(node);
                    }
                    "csv" => {
                        let filename = config
                            .get("filename")
                            .and_then(|v| v.as_str())
                            .unwrap_or("export.csv");
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.writeBinaryFile",
                            "typeVersion": 1,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "fileName": filename,
                                "dataPropertyName": "data",
                                "options": {}
                            }
                        });
                        nodes.push(node);
                    }
                    _ => {
                        // Generic HTTP POST export
                        let url = config.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.httpRequest",
                            "typeVersion": 4.2,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "method": "POST",
                                "url": url,
                                "sendBody": true,
                                "bodyParameters": {
                                    "parameters": [
                                        { "name": "data", "value": "={{ $json }}" }
                                    ]
                                }
                            }
                        });
                        nodes.push(node);
                    }
                }
            }

            "delay" | "wait" => {
                let duration_ms = config
                    .get("duration_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3600000); // default 1 hour
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.wait",
                    "typeVersion": 1,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "resume": "webhook",
                        "options": {
                            "maxTime": duration_ms
                        }
                    }
                });
                nodes.push(node);
            }

            "transform" | "code" => {
                let code = config
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("return $json;");
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.code",
                    "typeVersion": 2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "language": "javaScript",
                        "code": code,
                        "mode": "runOnceForAllItems"
                    }
                });
                nodes.push(node);
            }

            "render_video" | "render_media" | "render_image" | "render_audio" => {
                // Rendering step: calls the third-party provider's render API
                // to create content, then logs the result in account_renditions.
                let provider = config
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let api_endpoint = config
                    .get("endpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let method = config
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("POST");
                let asset_type = match step_type {
                    "render_video" => "video",
                    "render_image" => "image",
                    "render_audio" => "audio",
                    _ => "video",
                };

                // Node 1: Call the provider's API
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": method,
                        "url": api_endpoint,
                        "authentication": "none",
                        "sendHeaders": true,
                        "sendBody": true,
                        "options": {
                            "timeout": 120000
                        }
                    }
                });
                nodes.push(node);

                // Node 2: Log rendition via WorkflowSwift callback
                let log_id = format!("{}_log", node_id);
                // Build rendition payload from provider response
                let log_node = json!({
                    "id": log_id,
                    "name": format!("Log {} {}", provider, asset_type),
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos + 200, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": callback_url(callback_base_url, "renditions"),
                        "authentication": "none",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": CALLBACK_AUTH_EXPR },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "provider", "value": provider },
                                { "name": "provider_asset_id", "value": "={{ $json.id || $json.asset_id || $json.render_id || $json.video_id || $json.file_id }}" },
                                { "name": "provider_asset_url", "value": "={{ $json.url || $json.video_url || $json.asset_url || $json.generated_url || $json.download_url }}" },
                                { "name": "preview_url", "value": "={{ $json.preview_url || $json.thumbnail_url || $json.video_url || $json.url }}" },
                                { "name": "thumbnail_url", "value": "={{ $json.thumbnail_url || $json.thumbnail }}" },
                                { "name": "asset_type", "value": asset_type },
                                { "name": "step_name", "value": step_name },
                                { "name": "metadata", "value": "={{ $json }}" }
                            ]
                        }
                    }
                });
                nodes.push(log_node);

                // Wire render → log node
                let mut conn = serde_json::Map::new();
                conn.insert(
                    "main".to_string(),
                    json!([[{"node": log_id, "type": "main", "index": 0}]]),
                );
                connections_map.insert(node_id.to_string(), Value::Object(conn));

                // The log_id is the effective "output" node of this step
                step_last_node_id = Some(log_id);
            }

            "report" => {
                // Push data to dashboard widget
                let metric_key = config
                    .get("metric_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let period = config.get("period").and_then(|v| v.as_str()).unwrap_or("");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": callback_url(callback_base_url, "dashboard/push-widget-data"),
                        "authentication": "none",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": CALLBACK_AUTH_EXPR },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "metric_key", "value": metric_key },
                                { "name": "period", "value": period },
                                { "name": "value", "value": "={{ $json }}" }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            "trigger" | "poll" => {
                // Trigger/poll steps: these are handled at the n8n webhook/schedule level
                // For the execution pipeline, just pass through as a no-op transform node
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.noOp",
                    "typeVersion": 1,
                    "position": [x_pos, y_base],
                    "parameters": {}
                });
                nodes.push(node);
            }

            "fork" | "branch" => {
                let branches = config
                    .get("branches")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.len())
                    .unwrap_or(2);
                let mut output_connections = Vec::new();
                for b in 0..branches {
                    output_connections.push(json!({
                        "output": b,
                        "label": format!("Branch {}", b + 1)
                    }));
                }
                // Fork is represented as a Switch node in n8n
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.switch",
                    "typeVersion": 3,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "dataType": "number",
                        "value1": 1,
                        "rules": {
                            "conditions": [
                                {
                                    "id": "branch_1",
                                    "value1": "={{ $json }}",
                                    "operator": {
                                        "number": true,
                                        "operation": "exists"
                                    },
                                    "value2": ""
                                }
                            ]
                        },
                        "fallbackOutput": "",
                    }
                });
                nodes.push(node);
            }
            "generate" => {
                // RETIRED callback (kanban t_642b6894): this arm used to POST an app path
                // that has never been served, so the run died at the node (`onError:
                // stopWorkflow`). WorkflowSwift makes no LLM call: /provider-keys serves list/upsert/delete and {provider}/test only, and the engine's own generate arm posts to n8n. The step keeps
                // its place in the graph as a pass-through that NAMES what is missing -
                // the disposition the app's own engine applies to a step it cannot execute.
                nodes.push(passthrough_node(
                    &node_id,
                    step_name,
                    (x_pos, y_base),
                    step_type,
                    "WorkflowSwift makes no LLM call: /provider-keys serves list/upsert/delete and {provider}/test only, and the engine's own generate arm posts to n8n",
                ));
            }

            // ===== Format: Transform content for a specific platform =====
            "format" => {
                let platform = config
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("web");
                let content = config
                    .get("input_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("={{ $json }}");
                let format_type = config
                    .get("format_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto");

                // Use n8n Code node for formatting transformations
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.code",
                    "typeVersion": 2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "language": "javaScript",
                        "code": format!(r#"// Format step: {} platform
// Config format_type: {}
const input = $json;
const content = {};

// Apply platform-specific formatting
const output = {{
  original: content,
  platform: "{}",
  formatted: String(content),
  format_type: "{}",
  timestamp: new Date().toISOString(),
  metadata: {{
    char_count: String(content).length,
    platform: "{}"
  }}
}};

return output;
"#, step_name, format_type, content, platform, format_type, platform),
                        "mode": "runOnceForAllItems"
                    }
                });
                nodes.push(node);
            }
            "design" => {
                // RETIRED callback (kanban t_642b6894): this arm used to POST an app path
                // that has never been served, so the run died at the node (`onError:
                // stopWorkflow`). no design route exists in WorkflowSwift; the engine's design arm (src/execution.rs) returns a note and calls nothing. The step keeps
                // its place in the graph as a pass-through that NAMES what is missing -
                // the disposition the app's own engine applies to a step it cannot execute.
                nodes.push(passthrough_node(
                    &node_id,
                    step_name,
                    (x_pos, y_base),
                    step_type,
                    "no design route exists in WorkflowSwift; the engine's design arm (src/execution.rs) returns a note and calls nothing",
                ));
            }
            "publish" => {
                // RETIRED callback (kanban t_642b6894): this arm used to POST an app path
                // that has never been served, so the run died at the node (`onError:
                // stopWorkflow`). the /bridge command queue has no publish arm - its only consumer, the shipped Swift Market Intel extension, handles navigate/scrape/inject_script/notify/open_options. The step keeps
                // its place in the graph as a pass-through that NAMES what is missing -
                // the disposition the app's own engine applies to a step it cannot execute.
                nodes.push(passthrough_node(
                    &node_id,
                    step_name,
                    (x_pos, y_base),
                    step_type,
                    "the /bridge command queue has no publish arm - its only consumer, the shipped Swift Market Intel extension, handles navigate/scrape/inject_script/notify/open_options",
                ));
            }
            "loop" => {
                // RETIRED callback (kanban t_642b6894): this arm used to POST an app path
                // that has never been served, so the run died at the node (`onError:
                // stopWorkflow`). the app owns no loop state and /instances/loop-check has never existed; the app's own engine (src/execution.rs, the loop arm) records the iteration count and continues. The step keeps
                // its place in the graph as a pass-through that NAMES what is missing -
                // the disposition the app's own engine applies to a step it cannot execute.
                nodes.push(passthrough_node(
                    &node_id,
                    step_name,
                    (x_pos, y_base),
                    step_type,
                    "the app owns no loop state and /instances/loop-check has never existed; the app's own engine (src/execution.rs, the loop arm) records the iteration count and continues",
                ));
            }

            // ===== Condition: If/else routing =====
            "condition" => {
                let field = config
                    .get("field")
                    .and_then(|v| v.as_str())
                    .unwrap_or("$json");
                let operator = config
                    .get("operator")
                    .and_then(|v| v.as_str())
                    .unwrap_or("equals");
                let value = config
                    .get("value")
                    .cloned()
                    .unwrap_or(Value::String(String::new()));

                // One mapping, in Rust, from a step config to an operator pair the installed
                // n8n accepts (see `condition_operands`). The old arm emitted
                // `{"string": bool, "number": bool, "boolean": bool}`, which is not an
                // operator at all — every generated Condition node was invalid.
                let (left, right, operator_json) = condition_operands(field, operator, &value);

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.if",
                    "typeVersion": 2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "conditions": {
                            "combinator": "and",
                            "options": {
                                "caseSensitive": true,
                                "typeValidation": "strict"
                            },
                            "conditions": [
                                {
                                    "id": "cond_0",
                                    "leftValue": left,
                                    "rightValue": right,
                                    "operator": operator_json
                                }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            // ===== Manual Review: Pause for human approval =====
            "manual" => {
                let instructions = config
                    .get("instructions")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Review the workflow output and approve or reject.");
                let timeout_hours = config
                    .get("timeout_hours")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(24);

                // Manual review = n8n Wait node + callback to WorkflowSwift for notification
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.wait",
                    "typeVersion": 1,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "resume": "webhook",
                        "options": {
                            "maxTime": timeout_hours * 3600000
                        },
                        "notes": instructions
                    }
                });
                nodes.push(node);

                // RETIRED callback (kanban t_642b6894): this arm also POSTed
                // `/api/v1/notifications/manual-review`, a route that has never existed. A
                // mirror run creates no `workflow_instances` row, so the app's own approval
                // route (POST /instances/{id}/steps/{step_id}/decision) can never settle it -
                // a notification pointing at an approval that does not exist would be a lie.
                // The Wait node above is the mirror's honest hold; the app's engine keeps the
                // real gate (status `pending` until a decision arrives).
            }
            "research" => {
                // RETIRED callback (kanban t_642b6894): this arm used to POST an app path
                // that has never been served, so the run died at the node (`onError:
                // stopWorkflow`). every search/enrich/research handler in this app returns hardcoded mock data (prospecting/brand_monitor/competitor_watch all say 'Mock ... in production this would call external APIs'), so there is no real route to call. The step keeps
                // its place in the graph as a pass-through that NAMES what is missing -
                // the disposition the app's own engine applies to a step it cannot execute.
                nodes.push(passthrough_node(
                    &node_id,
                    step_name,
                    (x_pos, y_base),
                    step_type,
                    "every search/enrich/research handler in this app returns hardcoded mock data (prospecting/brand_monitor/competitor_watch all say 'Mock ... in production this would call external APIs'), so there is no real route to call",
                ));
            }

            _ => {
                // Unknown step type — add a comment/placeholder node
                let node = json!({
                    "id": node_id,
                    "name": format!("{} (unsupported)", step_name),
                    "type": "n8n-nodes-base.noOp",
                    "typeVersion": 1,
                    "position": [x_pos, y_base],
                    "parameters": {},
                    "notes": format!("WorkflowSwift step type '{}' could not be fully converted to n8n. Review manually.", step_type)
                });
                nodes.push(node);
            }
        }
        // Track this step's output node ID for external wiring
        step_output_ids.push(step_last_node_id.unwrap_or_else(|| node_id.clone()));
    }

    (nodes, step_output_ids)
}

/// Serialize the generated workflow to the JSON n8n's PUBLIC API accepts.
///
/// `POST {N8N_URL}/api/v1/workflows` validates the body against n8n's public
/// OpenAPI spec and is strict in both directions, so this payload is exactly the
/// accepted surface — measured against the live n8n 2.34.6 on this box, not guessed:
///   * required:      name, nodes, connections, settings
///   * accepted extra: staticData, nodeGroups, pinData
///   * rejected — "must NOT have additional properties": description
///   * rejected — read-only: id, versionId, active, createdAt, updatedAt,
///     isArchived, meta, tags
/// Sending more (as an earlier revision did) fails the whole mirror with
/// `400 request/body must NOT have additional properties`. n8n assigns the id,
/// versionId and timestamps itself and returns them in the 201/200 response.
pub fn to_n8n_json(wf: &N8nWorkflow) -> Value {
    json!({
        "name": wf.name,
        "nodes": wf.nodes,
        "connections": wf.connections,
        "settings": wf.settings,
        "staticData": null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://app.example.com";

    fn steps() -> Vec<Value> {
        // One step of every arm that builds an IF node, a callback node or a tenant-URL node.
        vec![
            json!({"step_type": "data-card", "name": "Data Card",
                   "config": {"widget_name": "Leads", "metric_key": "site-flipping_trends"}}),
            json!({"step_type": "condition", "name": "Enough?",
                   "config": {"field": "balance", "operator": "largerEqual", "value": "5"}}),
            json!({"step_type": "notify", "name": "Email ops",
                   "config": {"channel": "email", "recipient": "ops@example.com"}}),
            json!({"step_type": "http-request", "name": "Tenant API",
                   "config": {"url": "https://api.tenant.example/lead", "method": "POST"}}),
            json!({"step_type": "ai-action", "name": "OpenClaw", "config": {}}),
            json!({"step_type": "report", "name": "Report",
                   "config": {"metric_key": "weekly", "period": "7d"}}),
        ]
    }

    fn graph() -> N8nWorkflow {
        // Identifiers are generated, not literals: the gate rule this fleet runs on rejects
        // hardcoded UUID literals anywhere in src/ (the fleet has been burned by scripts that
        // hardcode a tenant id), and the converter's output does not depend on WHICH uuid it is
        // given — every assertion below is about node shape.
        convert_steps_to_n8n(&steps(), Uuid::new_v4(), Uuid::new_v4(), BASE)
    }

    fn is_app_callback(node: &Value) -> bool {
        node.get("parameters")
            .and_then(|p| p.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| u.starts_with(&format!("{}/api/v1/", BASE)))
            .unwrap_or(false)
    }

    fn header_names(node: &Value) -> Vec<String> {
        node.get("parameters")
            .and_then(|p| p.get("headerParameters"))
            .and_then(|h| h.get("parameters"))
            .and_then(|p| p.as_array())
            .map(|hs| {
                hs.iter()
                    .filter_map(|h| h.get("name").and_then(|n| n.as_str()))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn auth_header_value(node: &Value) -> Option<String> {
        node.get("parameters")
            .and_then(|p| p.get("headerParameters"))
            .and_then(|h| h.get("parameters"))
            .and_then(|p| p.as_array())?
            .iter()
            .find(|h| {
                h.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| n.eq_ignore_ascii_case("authorization"))
                    .unwrap_or(false)
            })
            .and_then(|h| h.get("value"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// The defect this card is about: `genericCredentialType` + `genericAuthType` with no
    /// credential attached makes n8n refuse the node ("Credentials not found") — measured on
    /// the live mirror, where the whole graph executed nothing.
    #[test]
    fn no_generated_node_demands_an_n8n_credential() {
        let g = graph();
        for node in &g.nodes {
            let s = node.to_string();
            assert!(
                !s.contains("genericCredentialType") && !s.contains("httpHeaderAuth"),
                "node {} demands a credential n8n will refuse: {}",
                node["name"],
                s
            );
        }
    }

    /// Every callback into THIS app carries the caller's own bearer, read from the WEBHOOK
    /// item — not `$json`, whose `.headers` only exists on the first callback node.
    #[test]
    fn every_app_callback_carries_the_callers_bearer_from_the_webhook_item() {
        let g = graph();
        let mut names: Vec<String> = Vec::new();
        for node in &g
            .nodes
            .iter()
            .filter(|n| is_app_callback(n))
            .collect::<Vec<_>>()
        {
            assert_eq!(
                node["parameters"]["authentication"],
                json!("none"),
                "{}",
                node["name"]
            );
            assert_eq!(
                auth_header_value(node).as_deref(),
                Some(CALLBACK_AUTH_EXPR),
                "{}",
                node["name"]
            );
            assert_eq!(
                header_names(node)
                    .iter()
                    .filter(|h| h.eq_ignore_ascii_case("authorization"))
                    .count(),
                1,
                "{} must carry exactly one Authorization header",
                node["name"]
            );
            assert_eq!(node["onError"], json!("stopWorkflow"), "{}", node["name"]);
            names.push(node["name"].as_str().unwrap_or_default().to_string());
        }
        // data-card + notify + ai-action-less callbacks + report + the three fixed nodes.
        assert!(
            names.len() >= 4,
            "expected the app callbacks to be covered, saw {names:?}"
        );
        for expected in ["Auth & Credit", "Deduct Credit", "Data Card", "Report"] {
            assert!(
                names.iter().any(|n| n == expected),
                "{expected} missing from {names:?}"
            );
        }
    }

    /// The bearer is this app's own JWT: it must never be attached to a tenant-supplied URL.
    #[test]
    fn tenant_urls_never_receive_the_app_bearer() {
        let g = graph();
        let tenant = g
            .nodes
            .iter()
            .find(|n| n["name"] == "Tenant API")
            .expect("the http-request step is in the graph");
        assert!(
            auth_header_value(tenant).is_none(),
            "the tenant's own URL must not receive this app's bearer"
        );
        assert!(
            tenant.get("onError").is_none(),
            "tenant nodes keep n8n's default error policy"
        );
    }

    /// Every IF the converter emits must use an operator pair the installed n8n accepts — the
    /// verified sets are in `filter_operator`'s doc comment. `largerEqual` is NOT one of them.
    #[test]
    fn every_if_node_uses_an_operator_this_n8n_accepts() {
        const NUMBER: [&str; 8] = [
            "empty",
            "notEmpty",
            "equals",
            "notEquals",
            "gt",
            "lt",
            "gte",
            "lte",
        ];
        const STRING: [&str; 12] = [
            "empty",
            "notEmpty",
            "equals",
            "notEquals",
            "contains",
            "notContains",
            "startsWith",
            "notStartsWith",
            "endsWith",
            "notEndsWith",
            "regex",
            "notRegex",
        ];
        const BOOLEAN: [&str; 6] = ["empty", "notEmpty", "true", "false", "equals", "notEquals"];
        let g = graph();
        let mut ifs = 0;
        for node in &g.nodes {
            if node["type"] != json!("n8n-nodes-base.if") {
                continue;
            }
            ifs += 1;
            let conds = &node["parameters"]["conditions"];
            assert_eq!(conds["combinator"], json!("and"), "{}", node["name"]);
            let list = conds["conditions"].as_array().expect("a conditions list");
            assert!(!list.is_empty(), "{}", node["name"]);
            for c in list {
                let op = &c["operator"];
                let (kind, operation) = (
                    op["type"].as_str().expect("operator.type"),
                    op["operation"].as_str().expect("operator.operation"),
                );
                let allowed: &[&str] = match kind {
                    "number" => &NUMBER,
                    "string" => &STRING,
                    "boolean" => &BOOLEAN,
                    other => panic!("unknown filter type {other} on {}", node["name"]),
                };
                assert!(
                    allowed.contains(&operation),
                    "{}: n8n 2.34.6 rejects {kind}:{operation}",
                    node["name"]
                );
                assert!(
                    c["leftValue"]
                        .as_str()
                        .expect("leftValue")
                        .starts_with("={{"),
                    "{}: leftValue must be an expression",
                    node["name"]
                );
            }
        }
        assert_eq!(ifs, 2, "Balance OK? and the condition step");
    }

    /// `/credits/balance` answers `{"balance":20,…}` — a flat object, never `data[0].balance`.
    #[test]
    fn balance_check_reads_the_response_the_endpoint_actually_returns() {
        let g = graph();
        let node = g
            .nodes
            .iter()
            .find(|n| n["name"] == "Balance OK?")
            .expect("Balance OK?");
        let left = node["parameters"]["conditions"]["conditions"][0]["leftValue"]
            .as_str()
            .unwrap();
        assert!(left.contains("$json.balance"), "{left}");
        assert!(
            !left.contains("data"),
            "the endpoint never returns a data array: {left}"
        );
    }

    /// The response card's value must be a DEFINED expression: an undefined body parameter is
    /// dropped by n8n and the app answers 400 "value required" (measured live).
    #[test]
    fn data_card_value_is_defined_by_default() {
        let g = graph();
        let card = g
            .nodes
            .iter()
            .find(|n| n["name"] == "Data Card")
            .expect("Data Card");
        let value = card["parameters"]["bodyParameters"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == json!("value"))
            .expect("the value parameter")["value"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(value, "={{ $json }}");
        let metric = card["parameters"]["bodyParameters"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == json!("metric_key"))
            .unwrap()["value"]
            .as_str()
            .unwrap();
        assert_eq!(metric, "site-flipping_trends");
    }

    #[test]
    fn condition_operands_map_every_supported_operator() {
        let cases: [(&str, &Value, &str, &str); 7] = [
            ("largerEqual", &json!("5"), "number", "gte"),
            ("larger", &json!("5"), "number", "gt"),
            ("smaller", &json!("5"), "number", "lt"),
            ("equals", &json!("20"), "number", "equals"),
            ("equals", &json!("paid"), "string", "equals"),
            ("contains", &json!("lead"), "string", "contains"),
            ("isTrue", &json!("true"), "boolean", "true"),
        ];
        for (operator, value, kind, operation) in cases {
            let (left, _, op) = condition_operands("balance", operator, value);
            assert_eq!(op["type"], json!(kind), "{operator}");
            assert_eq!(op["operation"], json!(operation), "{operator}");
            assert!(
                left.starts_with("={{") && left.ends_with("}}"),
                "{operator}: {left}"
            );
        }
    }

    /// A condition step's config can be EMPTY (the tenant console sends `{}` for it), and the
    /// emitted node still has to be a valid operator.
    #[test]
    fn an_empty_condition_config_still_yields_a_valid_operator() {
        let (left, right, op) = condition_operands("$json", "equals", &json!(""));
        assert_eq!(op["type"], json!("string"));
        assert_eq!(op["operation"], json!("equals"));
        assert_eq!(left, "={{ String($json ?? '') }}");
        assert_eq!(right, json!(""));
    }

    #[test]
    fn field_expression_accepts_bare_names_paths_and_ready_expressions() {
        assert_eq!(field_expression("balance"), "={{ $json[\"balance\"] }}");
        assert_eq!(field_expression("$json.balance"), "={{ $json.balance }}");
        assert_eq!(
            field_expression("={{ $json.balance }}"),
            "={{ $json.balance }}"
        );
        assert_eq!(field_expression(""), "={{ $json }}");
    }

    /// Every step_type `convert_user_steps` branches on, read out of this file's own source so the
    /// census list cannot drift from the code. The top-level arms of `match step_type` sit at
    /// twelve spaces of indentation; every `match` nested inside an arm is deeper, so it is
    /// excluded (see `the_arm_scan_reads_top_level_step_types_only`).
    fn converted_step_types() -> Vec<String> {
        let src = include_str!("n8n_converter.rs");
        let start = src
            .find("fn convert_user_steps(")
            .expect("the converter fn is in this file");
        let end = src
            .find("// Track this step's output node ID")
            .expect("the end of the arm block is in this file");
        let mut out: Vec<String> = Vec::new();
        for line in src[start..end].lines() {
            if !line.starts_with("            \"") || !line.contains("=>") {
                continue;
            }
            for part in line.split('"').skip(1).step_by(2) {
                let key = part.trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
                {
                    out.push(key.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The (step_type, config) matrix the census is driven with. The configs are declared; the URLs
    /// are not — they come out of the converter. The channel/destination variants are here because
    /// one step type can branch into several node shapes (`notify`, `export`).
    fn census_probes() -> Vec<(String, Value)> {
        let mut probes: Vec<(String, Value)> = Vec::new();
        for st in converted_step_types() {
            probes.push((st.clone(), json!({})));
            match st.as_str() {
                "notify" => {
                    for ch in ["email", "slack", "telegram", "webhook", "sms"] {
                        probes.push((
                            st.clone(),
                            json!({"channel": ch, "recipient": "ops@example.com"}),
                        ));
                    }
                }
                "export" => {
                    for d in ["google_sheets", "csv", "resend", "coreswift"] {
                        probes.push((st.clone(), json!({"destination": d})));
                    }
                }
                _ => {}
            }
        }
        probes
    }

    /// The acceptance for kanban t_642b6894: every callback URL the converter emits is generated
    /// HERE, from the converter, and checked live against the running container by
    /// `/opt/swift/audits/t_642b6894/10-callback-census.py` (evidence: `10-callback-census-*.txt`).
    /// A step type the app cannot execute must emit NO app callback — that half of the fix is what
    /// keeps the census green, and this test is where a new arm would break it first.
    #[test]
    fn app_callback_census_is_generated_from_the_converter() {
        // Mounted for real; verdicts in `10-callback-census-post.txt`. A new arm that adds a path
        // here has to add the route first.
        const SERVED: [&str; 4] = [
            "credits/balance",
            "credits/deduct",
            "dashboard/push-widget-data",
            "renditions",
        ];

        let mut rows: Vec<Value> = Vec::new();
        for (step_type, config) in census_probes() {
            let step = json!({
                "step_type": step_type,
                "name": format!("Step {step_type}"),
                "config": config,
            });
            let g = convert_steps_to_n8n(&[step], Uuid::new_v4(), Uuid::new_v4(), BASE);
            for node in g.nodes.iter().filter(|n| is_app_callback(n)) {
                rows.push(json!({
                    "step_type": step_type,
                    "node": node["name"],
                    "method": node["parameters"]["method"].as_str().unwrap_or("GET"),
                    "url": node["parameters"]["url"].as_str().unwrap_or(""),
                }));
            }
        }
        rows.sort_by_key(|r| r.to_string());
        rows.dedup();

        let out = std::env::var("WF_CALLBACK_CENSUS_OUT")
            .unwrap_or_else(|_| "/tmp/wf-callback-census.json".to_string());
        std::fs::write(&out, serde_json::to_string_pretty(&rows).unwrap())
            .unwrap_or_else(|e| panic!("writing {out}: {e}"));
        println!(
            "app-callback census: {} rows from {} probes over {} step types -> {out}",
            rows.len(),
            census_probes().len(),
            converted_step_types().len()
        );

        for row in &rows {
            let url = row["url"].as_str().unwrap_or("");
            let path = url
                .split_once(&format!("{BASE}/api/v1/"))
                .map(|(_, p)| p.to_string())
                .unwrap_or_else(|| panic!("a callback left this app's origin: {row}"));
            assert!(
                SERVED.contains(&path.as_str()),
                "the converter emits a callback to {path}, which the router does not serve: {row}"
            );
        }
        assert_eq!(
            rows.iter()
                .map(|r| r["url"].as_str().unwrap_or(""))
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            SERVED.len(),
            "expected exactly the {SERVED:?} callbacks: {rows:?}"
        );
    }

    /// The arm scan must read the arm heads and nothing else. A nested `match channel` /
    /// `match destination` inside an arm (`"email"`, `"csv"`, …) would otherwise be probed as a
    /// step type of its own and quietly widen the census.
    #[test]
    fn the_arm_scan_reads_top_level_step_types_only() {
        let types = converted_step_types();
        for expected in [
            "data-card",
            "notify",
            "condition",
            "manual",
            "research",
            "loop",
            "publish",
            "render_video",
        ] {
            assert!(
                types.contains(&expected.to_string()),
                "{expected} missing from {types:?}"
            );
        }
        for leak in [
            "email",
            "slack",
            "telegram",
            "csv",
            "google_sheets",
            "video",
            "image",
            "audio",
        ] {
            assert!(
                !types.contains(&leak.to_string()),
                "{leak} leaked out of a nested match: {types:?}"
            );
        }
        assert!(
            types.len() >= 20,
            "expected every arm, saw {}: {types:?}",
            types.len()
        );
    }

    /// The retirement itself: a step type this app cannot execute must not put an HTTP node in the
    /// graph pointing at an app path — that is the shape that killed every run reaching it.
    /// It keeps a node, because the graph's wiring depends on one.
    #[test]
    fn retired_step_types_pass_through_instead_of_calling_a_missing_route() {
        for st in [
            "loop",
            "generate",
            "design",
            "publish",
            "research",
            "alert",
            "score",
            "analyze",
            "search",
            "scrape",
            "enrich",
            "validation",
            "config",
            "init",
            "register",
            "test",
            "log",
        ] {
            let step = json!({"step_type": st, "name": format!("Step {st}"), "config": {}});
            let g = convert_steps_to_n8n(&[step], Uuid::new_v4(), Uuid::new_v4(), BASE);
            let mine: Vec<&Value> = g
                .nodes
                .iter()
                .filter(|n| {
                    n["name"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with(&format!("Step {st}"))
                })
                .collect();
            assert_eq!(
                mine.len(),
                1,
                "{st}: expected exactly one node, got {mine:?}"
            );
            let node = mine[0];
            assert!(
                !is_app_callback(node),
                "{st} still calls an app path the router does not serve: {node}"
            );
            assert!(
                !node["notes"].as_str().unwrap_or("").is_empty(),
                "{st}: the pass-through must name what is missing: {node}"
            );
        }
    }
}
