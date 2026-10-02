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
//!   - "notify"       → Webhook callback (n8n httpRequest, POST {message}); the only channel the console offers. `email` | `sms` are retired (kanban t_08be842f): no sender exists in this product, so a row stored before the retirement keeps its place as a no-op that names the gap
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
//!
//! Every graph also carries a FAILURE ARM (kanban t_07c33d98, arm (b)): an `Error Trigger` node
//! wired to a `Report Failure` HTTP node. n8n re-runs the same workflow in `mode: "error"` when
//! any node fails, so a failed external run of a mirrored workflow POSTs its own failure to
//! `POST /api/v1/n8n/run-outcome` and the app records a `failed` row where the tenant console
//! already lists runs. The trigger stays async — `responseMode` is deliberately NOT
//! `responseNode`: a graph containing a `wait`/`delay`/`manual` step would park the caller for
//! up to the wait's `maxTime`. The evidence for both the mechanism and that trade-off is in
//! `/opt/swift/audits/t_07c33d98/`.

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
        // The failure arm authenticates with the machine key, not the caller's bearer: its run
        // starts at the Error Trigger, so `$('Webhook')` was never executed and CALLBACK_AUTH_EXPR
        // would throw on exactly the path that exists to report a failure. Leave its own headers
        // (X-Internal-Key) alone — `failure_arm_nodes` already set authentication: none.
        if carries_internal_key(node) {
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

/// The name of the one node in a generated graph that reports a FAILED run back to this app
/// (kanban t_07c33d98, arm (b)).
pub const FAILURE_REPORT_NODE: &str = "Report Failure";

/// The error-arm trigger's node name. n8n runs a workflow's Error Trigger nodes — and the rest
/// of that workflow — when an execution fails (measured, see `failure_arm_nodes`).
pub const ERROR_TRIGGER_NODE: &str = "Error Trigger";

/// The header the failure-report node presents: this app's machine credential, NOT the caller's
/// bearer. On the failure arm the run starts at the Error Trigger, so `$('Webhook')` was never
/// executed and `CALLBACK_AUTH_EXPR` would throw on exactly the path it exists for.
pub const INTERNAL_KEY_HEADER: &str = "X-Internal-Key";

/// Does this node carry the machine key? Used by `harden_callback_nodes` to leave the failure
/// report's own headers alone — and by the unit tests, so the marker cannot drift.
fn carries_internal_key(node: &Value) -> bool {
    node.get("parameters")
        .and_then(|p| p.get("headerParameters"))
        .and_then(|h| h.get("parameters"))
        .and_then(|p| p.as_array())
        .map(|hs| {
            hs.iter()
                .any(|h| h.get("name").and_then(|n| n.as_str()) == Some(INTERNAL_KEY_HEADER))
        })
        .unwrap_or(false)
}

/// The failure arm: `Error Trigger → Report Failure`, wired to each other and to nothing else.
///
/// Measured on the fleet's n8n 2.34.6 before this was written
/// (`/opt/swift/audits/t_07c33d98/10-error-workflow-mechanism.txt`): a failed execution whose
/// workflow contains an `n8n-nodes-base.errorTrigger` node is re-run by n8n itself in
/// `mode: "error"` starting at that node, and its output IS n8n's `workflowErrorData`:
///
/// ```text
/// {"execution":{"id":"52","url":"…/executions/52",
///               "error":{"message":"The service refused the connection - perhaps it is offline",
///                        "httpCode":"ECONNREFUSED","node":{"name":"Boom"},…},
///               "lastNodeExecuted":"Boom","mode":"webhook","executionContext":{…}},
///  "workflow":{"id":"<n8n id>","name":"WFS <uuid>"}}
/// ```
///
/// That is deliberately preferred over the two other shapes:
///  * a separate error workflow (`settings.errorWorkflow`) — n8n refuses it unless that second
///    workflow is ACTIVE (`Workflow "<id>" is not active and cannot be executed`, read out of
///    the container's own log) and it needs one shared workflow provisioned out-of-band, so the
///    report would not travel with the graph it describes;
///  * `responseMode: "responseNode"` — it would make the CALLER wait for the whole run, and a
///    graph containing `wait`/`delay`/`manual` parks it for up to the wait's `maxTime`.
///
/// `onError: "continueRegularOutput"` on the report node keeps a broken report from ever
/// cascading: the run already failed, and this node IS the delivery of that fact.
fn failure_arm_nodes(
    callback_base_url: &str,
    internal_sync_key: &str,
    y: i32,
) -> (Vec<Value>, Vec<(String, Value)>) {
    let trigger = json!({
        "id": "error_trigger",
        "name": ERROR_TRIGGER_NODE,
        "type": "n8n-nodes-base.errorTrigger",
        "typeVersion": 1,
        "position": [250, y],
        "parameters": {}
    });
    let report = json!({
        "id": "failure_report",
        "name": FAILURE_REPORT_NODE,
        "type": "n8n-nodes-base.httpRequest",
        "typeVersion": 4.2,
        "position": [450, y],
        "onError": "continueRegularOutput",
        "parameters": {
            "method": "POST",
            "url": callback_url(callback_base_url, "n8n/run-outcome"),
            "sendBody": true,
            "specifyBody": "json",
            "jsonBody": "={{ JSON.stringify($json) }}",
            "authentication": "none",
            "sendHeaders": true,
            "headerParameters": {
                "parameters": [
                    { "name": INTERNAL_KEY_HEADER, "value": internal_sync_key }
                ]
            },
            "options": { "timeout": 10000 }
        }
    });
    let mut conn = serde_json::Map::new();
    conn.insert(
        "main".to_string(),
        json!([[{ "node": "failure_report", "type": "main", "index": 0 }]]),
    );
    (
        vec![trigger, report],
        vec![("error_trigger".to_string(), Value::Object(conn))],
    )
}

/// The HTTP method the generated mirror's OWN trigger registers (kanban t_d4dd6e42).
///
/// The Webhook node's default is **GET**, and the node this converter used to emit carried no
/// `httpMethod` at all, so the mirror's only entry point registered GET-only. Measured live on
/// n8n 2.34.6 (`/opt/swift/audits/t_70baf9b0/30-probe-post.txt`, §B3): a verbatim copy of the
/// emitted node imported and activated gave `webhook_entity -> ('…','GET','Webhook')`,
/// `POST /webhook/<path>` -> `404 {"code":404,"message":"This webhook is not registered for POST
/// requests. Did you mean to make a GET request?"}`, `GET /webhook/<path>` -> `200 {"message":
/// "Workflow was started"}`. The deploy route hands the tenant that path as "available for
/// external triggers", so the 404 landed on the caller the path exists for.
///
/// POST, and only POST: an external workflow trigger is a data-carrying call, this graph's
/// downstream nodes read the run item (`$json`), and a GET query string cannot carry it — every
/// machine-facing entry point in this fleet takes POST (this box's other n8n webhook, the
/// market-intel workflow, registers POST; so do the app's own receivers). A Webhook node at
/// typeVersion 1 takes ONE method; accepting GET as well would need a typeVersion-2 node, whose
/// other defaults are a change this card did not measure. Nothing calls the path today — 0 of the
/// 23 live `workflows` rows carry `n8n_webhook_path` and n8n holds 0 `wfs/` webhooks — so POST
/// breaks no existing caller.
pub const MIRROR_TRIGGER_METHOD: &str = "POST";

pub fn convert_steps_to_n8n(
    steps: &[Value],
    aid: Uuid,
    workflow_id: Uuid,
    callback_base_url: &str,
    internal_sync_key: &str,
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
            // Without this key n8n falls back to GET and a POST caller 404s (t_d4dd6e42).
            "httpMethod": MIRROR_TRIGGER_METHOD,
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

    // ===== Failure arm: Error Trigger → Report Failure =====
    // Separate row below the main chain, so a failed run reports itself where the tenant
    // console already lists runs. See `failure_arm_nodes`.
    let (failure_nodes, failure_conns) =
        failure_arm_nodes(callback_base_url, internal_sync_key, 900);
    nodes.extend(failure_nodes);
    for (src_id, conn) in failure_conns {
        connections_map.insert(src_id, conn);
    }

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
                    // `webhook` is the one channel `execution::NOTIFY_CHANNELS` accepts (kanban
                    // t_08be842f): a step stored without one, or with the retired `email` default,
                    // must not fall back to a channel this product cannot deliver on.
                    .unwrap_or("webhook");
                let recipient = config
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let message = config.get("message").and_then(|v| v.as_str()).unwrap_or("");
                // The step carried a `subject` field (draftConfig, www-app/index.html) while the
                // console still offered the Email channel; with `email` retired the console no
                // longer sends one and nothing here consumes it. A row stored before the
                // retirement may still carry the key — it is simply ignored.

                match channel {
                    // The tenant's `webhook` channel is an OUTBOUND call to a URL the tenant owns,
                    // so the node is `httpRequest` — the same shape the `http-request`/`export`
                    // arms emit. It used to emit an `n8n-nodes-base.webhook`, which is a TRIGGER:
                    // the emitted `method`/`url`/`sendBody`/`bodyParameters` are not Webhook-node
                    // parameters, so n8n registered the node as a stray GET webhook at a random
                    // UUID path, ran it with `executionStatus=success`, and posted NOTHING to the
                    // tenant's URL (measured on n8n 2.34.6, kanban t_70baf9b0).
                    "webhook" => {
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.httpRequest",
                            "typeVersion": 4.2,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "method": "POST",
                                "url": recipient,
                                "sendBody": true,
                                // The two fields the app's OWN notify arm puts on the wire
                                // (src/execution.rs: `message` from the step and `data`, the
                                // workflow's current item). The console's Notify step has no
                                // message field (channel | recipient | subject), so `message` is
                                // usually empty — `data` is what carries the run's payload, and
                                // without it this node posted `{"message":""}` to the tenant's
                                // endpoint, which is a notification that notifies nothing.
                                "bodyParameters": {
                                    "parameters": [
                                        { "name": "message", "value": message },
                                        { "name": "data", "value": "={{ $json }}" }
                                    ]
                                }
                            }
                        });
                        nodes.push(node);
                    }
                    // RETIRED ARMS (kanban t_70baf9b0, retired for good by t_08be842f): `email` and
                    // `sms` used to be offered by the tenant console's Notify step
                    // (www-app/index.html, Channel = email | webhook | sms). This app serves no
                    // sender for either, so those were channels a tenant could pick that deliver
                    // nothing; the console no longer offers them and the API refuses them
                    // (`execution::NOTIFY_CHANNELS`). These arms stay for a row stored before the
                    // retirement: an honest no-op that NAMES the gap, the same disposition
                    // src/execution.rs applies to a step it cannot execute.
                    //
                    // `email` used to emit `n8n-nodes-base.emailSend` with a hardcoded
                    // `fromEmail: swiftsoftware143@yahoo.com` and NO credential. The node type
                    // declares the `smtp` credential as REQUIRED, n8n holds no smtp credential at
                    // all, and the runtime activation path refuses the whole workflow:
                    // `Cannot publish workflow: 1 node have configuration issues: Node "<step>":
                    // Missing required credential: smtp` — measured, so ONE email Notify step
                    // makes the tenant's ENTIRE generated workflow un-activatable.
                    //
                    // The app DOES own a mail path (`email::send_email`, behind Admin > Settings >
                    // Email Provider), but it is template-based, it is not reachable from n8n, and
                    // relaying a tenant-named recipient through the platform's provider would make
                    // this product an open mail sender on its own domain. That recipient rule is a
                    // product/security decision (card t_08be842f, decided with the picker policy):
                    // until it is taken, the channel is not sold at all.
                    "email" => {
                        nodes.push(passthrough_node(
                            &node_id,
                            step_name,
                            (x_pos, y_base),
                            step_type,
                            "WorkflowSwift has no tenant-triggered mail sender: the platform's mail \
                             provider lives in the app (Admin > Settings > Email Provider) and no \
                             route relays a step's mail, so this Notify step sends nothing. The \
                             Email channel is RETIRED (kanban t_08be842f): the console no longer \
                             offers it and the API refuses it. This node only keeps the place of a \
                             step stored before the retirement.",
                        ));
                    }
                    "sms" => {
                        nodes.push(passthrough_node(
                            &node_id,
                            step_name,
                            (x_pos, y_base),
                            step_type,
                            "WorkflowSwift has no SMS provider: no sender in the app and no sms \
                             credential in n8n, so this Notify step sends nothing. The SMS channel \
                             is RETIRED (kanban t_08be842f): the console no longer offers it and the \
                             API refuses it. This node only keeps the place of a step stored before \
                             the retirement. (n8n does hold a telegramApi credential, but this \
                             product offers no telegram channel.)",
                        ));
                    }
                    // RETIRED callback (kanban t_642b6894): `slack` and `telegram` are not
                    // offered by the tenant console's Notify step (its Channel select is `webhook`,
                    // www-app/index.html) and WorkflowSwift serves no slack/telegram sender, so the
                    // arm posted a route that never existed and the run died there.
                    "slack" | "telegram" => {
                        nodes.push(passthrough_node(
                            &node_id,
                            step_name,
                            (x_pos, y_base),
                            step_type,
                            &format!("channel '{}' has no sender in WorkflowSwift (the console's Notify channel is webhook)", channel),
                        ));
                    }
                    _ => {
                        // An unknown channel used to emit a Webhook TRIGGER node, i.e. a stray
                        // unauthenticated GET endpoint that posted nothing. Name it instead.
                        nodes.push(passthrough_node(
                            &node_id,
                            step_name,
                            (x_pos, y_base),
                            step_type,
                            &format!("unknown notify channel '{}' (the console's Notify channel is webhook)", channel),
                        ));
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

/// Every destination a generated graph would CALL that is not one of this app's own callbacks —
/// i.e. every URL that came out of a step's config and is therefore the tenant's to choose.
///
/// WHY THIS EXISTS (kanban t_2741ac13): the destination gate the in-process engine runs
/// (`crate::security::webhook_security::gate_step_destination`) covers the engine only. A mirrored
/// graph is executed by n8n — a separate container — so the very same step config becomes a node
/// n8n fetches with nothing in front of it. Measured live, both legs: an `http-request` step pointed
/// at `http://127.0.0.1:18098/...` was refused in-process (0 sink hits), while the mirror of that
/// same workflow, once activated and triggered, fetched that loopback address from inside the n8n
/// container and returned the sink's body into the run (`/opt/swift/audits/t_2741ac13/`).
///
/// This is the choke point next to `harden_callback_nodes`: whatever a future arm emits, the caller
/// finds the URLs it has to gate HERE, so a new arm cannot add an ungated destination. Only
/// `n8n-nodes-base.httpRequest` nodes take a URL (the other emitted types — noOp, wait, if, switch,
/// code, webhook, errorTrigger, respondToWebhook, googleSheets, writeBinaryFile — carry none), and
/// this app's own callbacks are excluded by their exact origin prefix: they are generated here and
/// carry this app's bearer, never a tenant destination.
pub fn tenant_destinations(wf: &N8nWorkflow, callback_base_url: &str) -> Vec<(String, String)> {
    let prefix = format!("{}/api/v1/", callback_base_url.trim_end_matches('/'));
    let mut out = Vec::new();
    for node in &wf.nodes {
        if node.get("type").and_then(|t| t.as_str()) != Some("n8n-nodes-base.httpRequest") {
            continue;
        }
        let Some(url) = node
            .get("parameters")
            .and_then(|p| p.get("url"))
            .and_then(|u| u.as_str())
        else {
            continue;
        };
        if url.starts_with(&prefix) {
            continue;
        }
        out.push((
            node.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("(unnamed node)")
                .to_string(),
            url.to_string(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://app.example.com";

    /// Stands in for INTERNAL_SYNC_KEY: the machine credential the failure arm presents.
    const KEY: &str = "internal-sync-key-for-tests";

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
        convert_steps_to_n8n(&steps(), Uuid::new_v4(), Uuid::new_v4(), BASE, KEY)
    }

    /// The mirror is executed by n8n — a separate container — so the engine's destination gate does
    /// NOT cover the nodes it runs (kanban t_2741ac13). `tenant_destinations` is how the caller finds
    /// the URLs to gate, so this is the census that fails first when a new arm emits a destination
    /// nobody gates: every URL-bearing step hands its config URL over, and NONE of this app's own
    /// callbacks (generated here, carrying this app's bearer) is ever offered to the gate.
    #[test]
    fn tenant_destinations_are_the_step_configs_urls_and_never_this_apps_callbacks() {
        use std::collections::BTreeMap;
        let steps = vec![
            json!({"step_type": "http-request", "name": "Tenant API",
                   "config": {"method": "POST", "url": "https://api.tenant.example/lead"}}),
            json!({"step_type": "notify", "name": "Hook out",
                   "config": {"channel": "webhook", "recipient": "https://hooks.tenant.example/x"}}),
            json!({"step_type": "render_image", "name": "Render",
                   "config": {"provider": "probe", "endpoint": "https://render.tenant.example/api"}}),
            json!({"step_type": "ai-action", "name": "Agent",
                   "config": {"gateway_url": "https://gateway.tenant.example"}}),
            json!({"step_type": "export", "name": "Export",
                   "config": {"destination": "http", "url": "https://export.tenant.example/append"}}),
        ];
        let g = convert_steps_to_n8n(&steps, Uuid::new_v4(), Uuid::new_v4(), BASE, KEY);
        let got: BTreeMap<String, String> = tenant_destinations(&g, BASE).into_iter().collect();
        assert_eq!(
            got.len(),
            5,
            "one destination per url-bearing step: {got:?}"
        );
        assert_eq!(got["Tenant API"], "https://api.tenant.example/lead");
        assert_eq!(got["Hook out"], "https://hooks.tenant.example/x");
        assert_eq!(got["Render"], "https://render.tenant.example/api");
        assert_eq!(
            got["Agent"],
            "https://gateway.tenant.example/api/chat/completions"
        );
        assert_eq!(got["Export"], "https://export.tenant.example/append");
    }

    /// Same rule, swept over EVERY arm the converter emits (the probe list the callback census
    /// uses): no app callback and no run-time expression may ever be handed to the gate as a tenant
    /// destination.
    #[test]
    fn the_tenant_destination_census_excludes_app_callbacks_and_expressions() {
        for (step_type, config) in census_probes() {
            let step = json!({
                "step_type": step_type,
                "name": format!("Step {step_type}"),
                "config": config,
            });
            let g = convert_steps_to_n8n(&[step], Uuid::new_v4(), Uuid::new_v4(), BASE, KEY);
            for (node, url) in tenant_destinations(&g, BASE) {
                assert!(
                    !url.starts_with(BASE),
                    "{step_type}: {node} is this app's own callback, not a tenant destination: {url}"
                );
                assert!(
                    !url.contains("{{"),
                    "{step_type}: {node} carries an n8n expression, which no gate can read: {url}"
                );
            }
        }
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
            // The failure-report arm is the one deliberate exception: it presents the machine
            // key, not the caller's bearer (see the failure-arm tests below).
            .filter(|n| is_app_callback(n) && !carries_internal_key(n))
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
        const SERVED: [&str; 5] = [
            "credits/balance",
            "credits/deduct",
            "dashboard/push-widget-data",
            "renditions",
            // The failure arm's report route (kanban t_07c33d98): route and converter land
            // together, and this row is what would notice if either side moved alone.
            "n8n/run-outcome",
        ];

        let mut rows: Vec<Value> = Vec::new();
        for (step_type, config) in census_probes() {
            let step = json!({
                "step_type": step_type,
                "name": format!("Step {step_type}"),
                "config": config,
            });
            let g = convert_steps_to_n8n(&[step], Uuid::new_v4(), Uuid::new_v4(), BASE, KEY);
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
            let g = convert_steps_to_n8n(&[step], Uuid::new_v4(), Uuid::new_v4(), BASE, KEY);
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

    /// The failure arm (kanban t_07c33d98, arm (b)): every generated graph reports its OWN
    /// failure, and it presents the MACHINE credential — never the caller's bearer, which
    /// `harden_callback_nodes` adds to every node under this app's origin. On the error re-run
    /// the run STARTS at the Error Trigger, so `$('Webhook')` was never executed and that
    /// expression is precisely what would throw on the one path that has to work.
    #[test]
    fn the_failure_arm_reports_with_the_machine_key_not_the_callers_bearer() {
        let g = graph();
        let url = format!("{BASE}/api/v1/n8n/run-outcome");
        let reports: Vec<&Value> = g
            .nodes
            .iter()
            .filter(|n| n["parameters"]["url"].as_str().unwrap_or("") == url)
            .collect();
        assert_eq!(
            reports.len(),
            1,
            "exactly one failure report per graph: {reports:?}"
        );
        let report = reports[0];
        assert_eq!(report["name"], FAILURE_REPORT_NODE);
        assert_eq!(report["type"], "n8n-nodes-base.httpRequest");
        assert_eq!(report["parameters"]["method"], "POST");
        assert_eq!(report["parameters"]["authentication"], "none");
        // A broken report must never cascade: the run already failed, and this node IS the
        // delivery of that fact. `onError` is a NODE property in n8n, not a parameter.
        assert_eq!(report["onError"], "continueRegularOutput");
        assert_eq!(header_names(report), vec![INTERNAL_KEY_HEADER.to_string()]);
        let sent = report["parameters"]["headerParameters"]["parameters"][0]["value"]
            .as_str()
            .unwrap_or("");
        assert_eq!(sent, KEY, "the machine key, verbatim");
        let serialized = report.to_string();
        assert!(
            !serialized.contains("$('Webhook')"),
            "the failure arm must not read the Webhook item — it was never executed: {report}"
        );
        // n8n's own workflowErrorData, forwarded verbatim: that IS the error and the node.
        assert_eq!(
            report["parameters"]["jsonBody"],
            "={{ JSON.stringify($json) }}"
        );
    }

    /// The failure arm is wired to itself and to nothing else, and the caller contract stays
    /// ASYNC — `responseMode: responseNode` is the arm this card rejected (a graph containing a
    /// `wait`/`delay`/`manual` step would park the caller for up to the wait's `maxTime`).
    #[test]
    fn the_error_trigger_drives_only_the_failure_report_and_the_trigger_stays_async() {
        let g = graph();
        let triggers: Vec<&Value> = g
            .nodes
            .iter()
            .filter(|n| n["type"] == "n8n-nodes-base.errorTrigger")
            .collect();
        assert_eq!(triggers.len(), 1, "one Error Trigger per graph");
        assert_eq!(triggers[0]["name"], ERROR_TRIGGER_NODE);

        let conns = g.connections.as_object().expect("connections object");
        let driven = conns
            .get(ERROR_TRIGGER_NODE)
            .expect("the Error Trigger drives the report node");
        assert_eq!(driven["main"][0][0]["node"], FAILURE_REPORT_NODE);
        assert_eq!(driven["main"].as_array().unwrap().len(), 1);
        assert!(
            !conns.contains_key(FAILURE_REPORT_NODE),
            "the report node is a leaf: {conns:?}"
        );

        let webhook = g
            .nodes
            .iter()
            .find(|n| n["type"] == "n8n-nodes-base.webhook")
            .expect("webhook trigger");
        assert_ne!(
            webhook["parameters"]["responseMode"], "responseNode",
            "the caller contract must stay async (t_07c33d98 decision)"
        );
        assert!(
            webhook["parameters"]["responseMode"].is_null(),
            "no responseMode at all: n8n's onReceived default answers 200 immediately"
        );
    }

    /// kanban t_d4dd6e42 — the generated mirror's ONLY entry point must register as POST.
    ///
    /// The Webhook node's default is GET, so the node this converter emitted without `httpMethod`
    /// registered a GET-only webhook on the live n8n 2.34.6: `webhook_entity` ->
    /// `('zzprobe-…','GET','Webhook')`, `POST /webhook/<path>` -> 404 "This webhook is not
    /// registered for POST requests", `GET /webhook/<path>` -> 200 + a real execution
    /// (`/opt/swift/audits/t_70baf9b0/30-probe-post.txt`, §B3). The path is what the deploy route
    /// hands a tenant and stores in `lifecycle_summary` as the external trigger, so the 404 landed
    /// on exactly the caller the path exists for.
    ///
    /// Pinned three ways, because the defect was a MISSING key and not a wrong value: the method is
    /// present, it is literally `POST`, and the node stays at typeVersion 1 — a silent upgrade to a
    /// v2 node (the only way to accept GET as well) must break this test rather than ship.
    #[test]
    fn the_generated_trigger_registers_post_and_only_post() {
        let g = graph();

        let triggers: Vec<&Value> = g
            .nodes
            .iter()
            .filter(|n| n["type"] == "n8n-nodes-base.webhook")
            .collect();
        assert_eq!(triggers.len(), 1, "one trigger per graph: {triggers:?}");
        let t = triggers[0];
        assert_eq!(t["id"], "webhook");
        assert_eq!(t["name"], "Webhook");

        // The presence of the key IS the fix: n8n's Webhook node defaults to GET.
        assert_eq!(
            t["parameters"]["httpMethod"],
            json!(MIRROR_TRIGGER_METHOD),
            "the trigger must name its method — n8n's default is GET"
        );
        assert_eq!(
            t["parameters"]["httpMethod"],
            json!("POST"),
            "POST is the decision (t_d4dd6e42): external triggers carry data"
        );
        assert!(
            t["parameters"]["httpMethod"].is_string(),
            "a multi-method accept is an ARRAY and needs a typeVersion-2 node",
        );
        assert_eq!(
            t["typeVersion"],
            json!(1),
            "v1 is the node this fleet measured; v2 changes other defaults"
        );

        // The path stays the one the deploy route returns and stores.
        let path = t["parameters"]["path"].as_str().unwrap_or("");
        assert!(
            path.starts_with("wfs/"),
            "namespaced trigger path, got {path}"
        );

        // The trigger is the graph's entry point, not a decorative node: it drives the first
        // callback node, so this IS the node whose method a caller hits.
        let conns = g.connections.as_object().expect("connections object");
        let driven = conns.get("Webhook").expect("the trigger drives the graph");
        assert_eq!(driven["main"][0][0]["node"], "Auth & Credit");
    }

    /// kanban t_70baf9b0 — the Notify step's n8n nodes, pinned to the shapes measured live on
    /// n8n 2.34.6 (audits/t_70baf9b0/):
    ///
    /// * `email` emitted `n8n-nodes-base.emailSend` (hardcoded `fromEmail`, NO credential). The
    ///   node type declares `smtp` as a REQUIRED credential and n8n holds none, so the runtime
    ///   activation path refused the WHOLE workflow: `Cannot publish workflow: 1 node have
    ///   configuration issues: Node "<step>": Missing required credential: smtp`. One Email
    ///   Notify step therefore made a tenant's entire generated workflow un-activatable.
    /// * `webhook`/`sms`/unknown emitted `n8n-nodes-base.webhook` — a TRIGGER. n8n activated the
    ///   graph anyway, reported `executionStatus=success`, posted NOTHING to the declared URL,
    ///   and registered a stray unauthenticated GET webhook at a random UUID path.
    ///
    /// The webhook channel is a real outbound call (httpRequest); email and sms have no sender in
    /// this product, so they are honest no-ops that NAME the gap.
    #[test]
    fn notify_channels_emit_nodes_that_can_actually_run() {
        let steps = vec![
            json!({"step_type": "notify", "name": "Notify Email",
                   "config": {"channel": "email", "recipient": "ops@example.com", "subject": "s"}}),
            json!({"step_type": "notify", "name": "Notify Webhook",
                   "config": {"channel": "webhook", "recipient": "https://hooks.tenant.example/incoming"}}),
            json!({"step_type": "notify", "name": "Notify SMS",
                   "config": {"channel": "sms", "recipient": "+15551234567"}}),
            json!({"step_type": "notify", "name": "Notify Unknown",
                   "config": {"channel": "carrier-pigeon", "recipient": "x"}}),
        ];
        let g = convert_steps_to_n8n(&steps, Uuid::new_v4(), Uuid::new_v4(), BASE, KEY);
        let by_name = |n: &str| {
            g.nodes
                .iter()
                .find(|x| x["name"] == n)
                .unwrap_or_else(|| panic!("no node named {n}"))
        };

        // Nothing in the graph may require a credential this n8n does not hold (it holds exactly
        // telegramApi and postgres): that is what made the whole graph un-activatable.
        for node in &g.nodes {
            assert_ne!(
                node["type"],
                json!("n8n-nodes-base.emailSend"),
                "{}",
                node["name"]
            );
            assert!(node.get("credentials").is_none(), "{}", node["name"]);
        }
        assert!(
            !serde_json::to_string(&g.nodes)
                .unwrap()
                .contains("swiftsoftware143"),
            "the hardcoded fromEmail is gone with the mail node"
        );

        // webhook → the OUTBOUND node, POSTing the message body to the tenant's own URL, and
        // never this app's bearer (the tenant URL is not an app callback).
        let hook = by_name("Notify Webhook");
        assert_eq!(hook["type"], json!("n8n-nodes-base.httpRequest"));
        assert_eq!(hook["parameters"]["method"], json!("POST"));
        assert_eq!(
            hook["parameters"]["url"],
            json!("https://hooks.tenant.example/incoming")
        );
        assert_eq!(hook["parameters"]["sendBody"], json!(true));
        assert_eq!(
            hook["parameters"]["bodyParameters"]["parameters"][0]["name"],
            json!("message")
        );
        assert_eq!(
            hook["parameters"]["bodyParameters"]["parameters"][1],
            json!({"name": "data", "value": "={{ $json }}"}),
            "the tenant's endpoint must receive the run's data, not only an empty message"
        );
        assert!(
            auth_header_value(hook).is_none(),
            "the tenant's own URL must not receive this app's bearer"
        );
        assert!(!is_app_callback(hook));

        // email | sms | unknown channel → an honest no-op that NAMES the gap. The first two now also
        // say the channel itself is RETIRED (kanban t_08be842f): the console no longer offers them
        // and the write path refuses them, so only a row stored before the retirement can reach it.
        for (name, needle) in [
            ("Notify Email", "no tenant-triggered mail sender"),
            ("Notify SMS", "no SMS provider"),
            ("Notify Unknown", "unknown notify channel"),
        ] {
            let n = by_name(name);
            assert_eq!(n["type"], json!("n8n-nodes-base.noOp"), "{name}");
            assert!(
                n["notes"].as_str().unwrap_or("").contains(needle),
                "{name} must name the gap, got {}",
                n["notes"]
            );
        }
        for retired in ["Notify Email", "Notify SMS"] {
            let n = by_name(retired);
            assert!(
                n["notes"].as_str().unwrap_or("").contains("RETIRED"),
                "{retired} must say the channel is retired, got {}",
                n["notes"]
            );
            assert!(
                !crate::execution::is_notify_channel(if retired.ends_with("Email") {
                    "email"
                } else {
                    "sms"
                }),
                "{retired} carries a channel the API must refuse"
            );
        }
        // The one channel the console still offers is the one the vocabulary accepts.
        assert!(crate::execution::is_notify_channel("webhook"));

        // Exactly ONE Webhook node — the graph's own trigger. A second one is a stray
        // unauthenticated endpoint that does the step's work nowhere.
        let webhooks = g
            .nodes
            .iter()
            .filter(|n| n["type"] == json!("n8n-nodes-base.webhook"))
            .count();
        assert_eq!(
            webhooks, 1,
            "only the graph's own trigger may be a Webhook node"
        );
    }
}
