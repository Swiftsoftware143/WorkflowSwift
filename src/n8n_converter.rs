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
//!   - "notify"       → Email / Slack / Telegram
//!   - "delay"        → n8n Wait node
//!   - "fork"         → n8n Switch node (parallel branches)
//!   - "action"       → Generic API call
//!   - "transform"    → n8n Code / Set node
//!   - "openclaw"     → OpenClaw reasoning step

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
///
/// `steps` — a list of JSON objects, each with at minimum:
///   { "step_type": "...", "name": "...", "config": { ... } }
///
/// `aid` is used for webhook path namespacing.
/// `workflow_id` is the UUID of the WorkflowSwift workflow.
/// `workflow_name` is used as the n8n workflow title.
/// `callback_base_url` is the base URL for callbacks (e.g. "http://workflowswift:8085").
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
            "url": format!("{}/api/credits/balance", callback_base_url.trim_end_matches('/')),
            "authentication": "genericCredentialType",
            "genericAuthType": "httpHeaderAuth",
            "sendHeaders": true,
            "headerParameters": {
                "parameters": [
                    {
                        "name": "Authorization",
                        "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}"
                    }
                ]
            }
        }
    });
    nodes.push(credit_node);

    // ===== Node 2: Balance Check =====
    let balance_check_node = json!({
        "id": "balance_check",
        "name": "Balance OK?",
        "type": "n8n-nodes-base.if",
        "typeVersion": 2,
        "position": [650, 300],
        "parameters": {
            "conditions": {
                "options": {
                    "caseSensitive": true,
                    "typeValidation": "strict"
                },
                "conditions": [
                    {
                        "id": "has_balance",
                        "leftValue": "={{ $json[\"data\"] && $json[\"data\"][0] && $json[\"data\"][0].balance }}",
                        "rightValue": 1,
                        "operator": {
                            "type": "number",
                            "operation": "largerEqual"
                        }
                    }
                ]
            }
        }
    });
    nodes.push(balance_check_node);

    // Build connection chains
    let mut prev_node_id = "balance_check";
    let mut prev_output_index = 0; // 0 = success branch, 1 = failure branch

    // ===== Node 3: Deduct Credit =====
    let deduct_node = json!({
        "id": "deduct_credit",
        "name": "Deduct Credit",
        "type": "n8n-nodes-base.httpRequest",
        "typeVersion": 4.2,
        "position": [850, 200],
        "parameters": {
            "method": "POST",
            "url": format!("{}/api/credits/deduct", callback_base_url.trim_end_matches('/')),
            "authentication": "genericCredentialType",
            "genericAuthType": "httpHeaderAuth",
            "sendHeaders": true,
            "headerParameters": {
                "parameters": [
                    {
                        "name": "Authorization",
                        "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}"
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
    let (step_nodes, step_output_ids) = convert_user_steps(steps, aid, workflow_id, callback_base_url, &mut connections_map);

    // Wire Webhook → Credit Check
    let mut webhook_conn = serde_json::Map::new();
    webhook_conn.insert("main".to_string(), json!([[{"node": "credit_check", "type": "main", "index": 0}]]));
    connections_map.insert("webhook".to_string(), Value::Object(webhook_conn));

    // Wire Credit Check → Balance Check
    let mut credit_conn = serde_json::Map::new();
    credit_conn.insert("main".to_string(), json!([[{"node": "balance_check", "type": "main", "index": 0}]]));
    connections_map.insert("credit_check".to_string(), Value::Object(credit_conn));

    // Wire Balance Check success → Deduct Credit; failure → respond with error
    let mut balance_conn = serde_json::Map::new();
    balance_conn.insert("main".to_string(), json!([
        [{"node": "deduct_credit", "type": "main", "index": 0}],
        [{"node": "respond"}]
    ]));
    connections_map.insert("balance_check".to_string(), Value::Object(balance_conn));

    // Wire Deduct → first user step's output node
    if let Some(first_output) = step_output_ids.first() {
        let mut deduct_conn = serde_json::Map::new();
        deduct_conn.insert("main".to_string(), json!([[{"node": first_output, "type": "main", "index": 0}]]));
        connections_map.insert("deduct_credit".to_string(), Value::Object(deduct_conn));
    }

    // Wire user steps in sequence using step_output_ids
    for i in 0..step_output_ids.len() {
        let current_id = &step_output_ids[i];

        if i + 1 < step_output_ids.len() {
            let next_id = &step_output_ids[i + 1];
            let mut conn = serde_json::Map::new();
            conn.insert("main".to_string(), json!([[{"node": next_id, "type": "main", "index": 0}]]));
            connections_map.insert(current_id.clone(), Value::Object(conn));
        } else {
            // Last user step → Respond node
            let mut conn = serde_json::Map::new();
            conn.insert("main".to_string(), json!([[{"node": "respond", "type": "main", "index": 0}]]));
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

    // Settings
    let settings = json!({
        "timezone": "America/New_York",
        "saveDataErrorExecution": "all",
        "saveDataSuccessExecution": "all",
        "saveManualExecutions": true,
        "callerPolicy": "workflowsWithSameOwner",
    });

    N8nWorkflow {
        name: format!("WFS {}", workflow_id.to_string()),
        nodes,
        connections: Value::Object(connections_map),
        settings,
        webhook_path: webhook_path.clone(),
    }
}

fn get_node_id(node: &Value) -> String {
    node.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
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
    let aid_prefix = &aid.to_string()[..8];
    let _wf_short = &workflow_id.to_string()[..8];

    for (i, step) in steps.iter().enumerate() {
        let step_type = step.get("step_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let step_name = step.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Step");
        let config = step.get("config").and_then(|v| v.as_object()).cloned().unwrap_or_default();

        let x_pos = 250 + (i as i32 + 1) * 200;
        let y_base = 300;
        let node_id = format!("step_{}", i);

        // Track whether this step produced a custom last-node ID (for multi-node steps)
        let mut step_last_node_id: Option<String> = None;

        match step_type {
            "http-request" | "action" => {
                let method = config.get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET");
                let url = config.get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

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
                        "sendHeaders": !config.get("headers").is_none(),
                        "sendBody": method != "GET",
                    }
                });
                nodes.push(node);
            }

            "ai-action" | "openclaw" => {
                let prompt = config.get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let model = config.get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("deepseek/deepseek-chat");

                // OpenClaw HTTP node — POST to the gateway
                // The user provides their OpenClaw gateway URL in Integration Center
                let openclaw_url = config.get("gateway_url")
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
                let metric_key = config.get("metric_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let value_expr = config.get("value_expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("={{ $json[\"data\"] }}");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/dashboard/push-widget-data", callback_base_url.trim_end_matches('/')),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
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
                let channel = config.get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("email");
                let recipient = config.get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let subject = config.get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("WorkflowSwift Notification");
                let message = config.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

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
                    "slack" | "telegram" => {
                        let node = json!({
                            "id": node_id,
                            "name": step_name,
                            "type": "n8n-nodes-base.httpRequest",
                            "typeVersion": 4.2,
                            "position": [x_pos, y_base],
                            "parameters": {
                                "method": "POST",
                                "url": format!("{}/api/notifications/{}", callback_base_url.trim_end_matches('/'), channel),
                                "authentication": "genericCredentialType",
                                "genericAuthType": "httpHeaderAuth",
                                "sendHeaders": true,
                                "headerParameters": {
                                    "parameters": [
                                        { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" }
                                    ]
                                },
                                "sendBody": true,
                                "bodyParameters": {
                                    "parameters": [
                                        { "name": "to", "value": recipient },
                                        { "name": "message", "value": message }
                                    ]
                                }
                            }
                        });
                        nodes.push(node);
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
                let destination = config.get("destination")
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
                        let filename = config.get("filename")
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
                        let url = config.get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
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
                let duration_ms = config.get("duration_ms")
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
                let code = config.get("code")
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
                let provider = config.get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let api_endpoint = config.get("endpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let method = config.get("method")
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
                        "url": format!("{}/api/v1/renditions", callback_base_url.trim_end_matches('/')),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
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
                conn.insert("main".to_string(), json!([[{"node": log_id, "type": "main", "index": 0}]]));
                connections_map.insert(node_id.to_string(), Value::Object(conn));

                // The log_id is the effective "output" node of this step
                step_last_node_id = Some(log_id);
            }

            "fork" | "branch" => {
                let branches = config.get("branches")
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

            // ===== Generate: LLM text/image generation (calls OpenAI/Anthropic) =====
            "generate" => {
                let provider = config.get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("openai");
                let prompt = config.get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let model = config.get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("gpt-4");

                // Route through WorkflowSwift's provider-key resolution
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/v1/provider-keys/{}/generate",
                            callback_base_url.trim_end_matches('/'), provider),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "model", "value": model },
                                { "name": "prompt", "value": prompt },
                                { "name": "temperature", "value": config.get("temperature").and_then(|v| v.as_f64()).unwrap_or(0.7) }
                            ]
                        },
                        "options": {
                            "timeout": 120000
                        }
                    }
                });
                nodes.push(node);
            }

            // ===== Format: Transform content for a specific platform =====
            "format" => {
                let platform = config.get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("web");
                let content = config.get("input_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("={{ $json }}");
                let format_type = config.get("format_type")
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

            // ===== Design: Create visuals/assets (calls provider API) =====
            "design" => {
                let provider = config.get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("artistly");
                let prompt = config.get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let design_type = config.get("design_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("graphic");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/v1/provider-keys/{}/design/{}",
                            callback_base_url.trim_end_matches('/'), provider, design_type),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "prompt", "value": prompt },
                                { "name": "design_type", "value": design_type },
                                { "name": "style", "value": config.get("style").and_then(|v| v.as_str()).unwrap_or("modern") },
                                { "name": "size", "value": config.get("size").and_then(|v| v.as_str()).unwrap_or("1024x1024") }
                            ]
                        },
                        "options": {
                            "timeout": 180000
                        }
                    }
                });
                nodes.push(node);
            }

            // ===== Publish: Post via Buffer (white-hat only) =====
            "publish" => {
                let platform = config.get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("twitter");
                let content = config.get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("={{ $json }}");
                let schedule_time = config.get("schedule_time")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // White-hat: always routes through Buffer API
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/v1/bridge/commands/publish",
                            callback_base_url.trim_end_matches('/')),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "platform", "value": platform },
                                { "name": "content", "value": content },
                                { "name": "schedule_time", "value": schedule_time }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            // ===== Loop: Repeat steps until condition =====
            "loop" => {
                let max_iterations = config.get("max_iterations")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5);
                let condition_field = config.get("condition_field")
                    .and_then(|v| v.as_str())
                    .unwrap_or("$json");
                let stop_value = config.get("stop_value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Loop is represented as an n8n Switch + Trigger combination
                // First: an IF node to check loop continuation condition
                // The actual iteration count management happens via WorkflowSwift callback
                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/v1/instances/loop-check",
                            callback_base_url.trim_end_matches('/')),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "max_iterations", "value": max_iterations },
                                { "name": "condition_field", "value": condition_field },
                                { "name": "stop_value", "value": stop_value },
                                { "name": "current_data", "value": "={{ $json }}" }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            // ===== Condition: If/else routing =====
            "condition" => {
                let field = config.get("field")
                    .and_then(|v| v.as_str())
                    .unwrap_or("$json");
                let operator = config.get("operator")
                    .and_then(|v| v.as_str())
                    .unwrap_or("equals");
                let value = config.get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Determine the operator type in Rust (not in JSON template)
                let is_string_op = matches!(operator, "equals" | "contains" | "startsWith" | "endsWith");
                let is_number_op = matches!(operator, "larger" | "smaller" | "equals");
                let is_boolean_op = operator == "isTrue" || operator == "isFalse";

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.if",
                    "typeVersion": 2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "conditions": {
                            "options": {
                                "caseSensitive": true,
                                "typeValidation": "strict"
                            },
                            "conditions": [
                                {
                                    "id": "cond_0",
                                    "leftValue": field,
                                    "rightValue": value,
                                    "operator": {
                                        "string": is_string_op,
                                        "number": is_number_op,
                                        "boolean": is_boolean_op
                                    }
                                }
                            ]
                        }
                    }
                });
                nodes.push(node);
            }

            // ===== Manual Review: Pause for human approval =====
            "manual" => {
                let instructions = config.get("instructions")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Review the workflow output and approve or reject.");
                let timeout_hours = config.get("timeout_hours")
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

                // Also send a notification to the user that manual review is needed
                let notify_id = format!("{}_notify", node_id);
                let notify_node = json!({
                    "id": notify_id,
                    "name": format!("Notify: {}", step_name),
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos + 200, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/v1/notifications/manual-review",
                            callback_base_url.trim_end_matches('/')),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "type", "value": "manual_review" },
                                { "name": "instructions", "value": instructions },
                                { "name": "timeout_hours", "value": timeout_hours }
                            ]
                        }
                    }
                });
                nodes.push(notify_node);

                // Wire wait → notify
                let mut conn = serde_json::Map::new();
                conn.insert("main".to_string(), json!([[{"node": notify_id, "type": "main", "index": 0}]]));
                connections_map.insert(node_id.to_string(), Value::Object(conn));

                step_last_node_id = Some(notify_id);
            }

            // ===== Research: Data scraping/enrichment (Hexomatic, Google Places, Apollo) =====
            "research" => {
                let provider = config.get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hexomatic");
                let query = config.get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let research_type = config.get("research_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("search");

                let node = json!({
                    "id": node_id,
                    "name": step_name,
                    "type": "n8n-nodes-base.httpRequest",
                    "typeVersion": 4.2,
                    "position": [x_pos, y_base],
                    "parameters": {
                        "method": "POST",
                        "url": format!("{}/api/v1/provider-keys/{}/research/{}",
                            callback_base_url.trim_end_matches('/'), provider, research_type),
                        "authentication": "genericCredentialType",
                        "genericAuthType": "httpHeaderAuth",
                        "sendHeaders": true,
                        "headerParameters": {
                            "parameters": [
                                { "name": "Authorization", "value": "=Bearer {{ $json.headers.authorization.split(' ')[1] }}" },
                                { "name": "Content-Type", "value": "application/json" }
                            ]
                        },
                        "sendBody": true,
                        "bodyParameters": {
                            "parameters": [
                                { "name": "query", "value": query },
                                { "name": "research_type", "value": research_type },
                                { "name": "params", "value": "={{ $json }}" }
                            ]
                        },
                        "options": {
                            "timeout": 180000
                        }
                    }
                });
                nodes.push(node);
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

/// Serialize the generated workflow to n8n-compatible JSON.
pub fn to_n8n_json(wf: &N8nWorkflow) -> Value {
    json!({
        "name": wf.name,
        "nodes": wf.nodes,
        "connections": wf.connections,
        "settings": wf.settings,
        "versionId": Uuid::new_v4().to_string(),
        "active": false,
    })
}
