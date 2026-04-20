//! MCP (Model Context Protocol) JSON-RPC handlers.
//!
//! Routes:
//! - `POST /mcp/{module}` — MCP JSON-RPC (`tools/list`, `tools/call`)
//! - `GET  /mcp/{module}/sse` — MCP SSE transport (event stream)

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use chatty_wasm_runtime::ToolDefinition;
use serde_json::{Value, json};

use crate::gateway::GatewayState;

use super::jsonrpc::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND,
    json_rpc_error, json_rpc_ok, module_not_found, module_not_found_json,
};

// ---------------------------------------------------------------------------
// Handler: POST /mcp/{module}
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_jsonrpc(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
    Json(body): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if body.jsonrpc != "2.0" {
        return json_rpc_error(
            StatusCode::BAD_REQUEST,
            body.id,
            INVALID_REQUEST,
            "Invalid Request: jsonrpc must be \"2.0\"",
        );
    }

    match body.method.as_str() {
        // MCP lifecycle
        "initialize" => handle_initialize(&module_name, body.id, &state).await,
        "notifications/initialized" | "initialized" => (StatusCode::ACCEPTED, "").into_response(),
        method if method.starts_with("notifications/") => {
            (StatusCode::ACCEPTED, "").into_response()
        }
        "ping" => json_rpc_ok(body.id, json!({})),
        // MCP tool methods
        "tools/list" => handle_tools_list(&module_name, body.id, &state).await,
        "tools/call" => handle_tools_call(&module_name, body.id, body.params, &state).await,
        method => json_rpc_error(
            StatusCode::OK,
            body.id,
            METHOD_NOT_FOUND,
            format!("Method not found: {}", method),
        ),
    }
}

async fn handle_initialize(
    module_name: &str,
    id: Option<Value>,
    state: &GatewayState,
) -> axum::response::Response {
    let reg = state.registry.read().await;
    if reg.get(module_name).is_none() {
        return module_not_found(id, module_name);
    }
    drop(reg);

    json_rpc_ok(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {
                "name": module_name,
                "version": "0.1.0"
            },
            "capabilities": {
                "tools": { "listChanged": false }
            }
        }),
    )
}

async fn handle_tools_list(
    module_name: &str,
    id: Option<Value>,
    state: &GatewayState,
) -> axum::response::Response {
    let mut reg = state.registry.write().await;
    let module = match reg.get_mut(module_name) {
        Some(m) => m,
        None => {
            return module_not_found(id, module_name);
        }
    };

    match module.list_tools() {
        Ok(tools) => {
            let tool_list: Vec<Value> = tools.iter().map(tool_to_json).collect();
            json_rpc_ok(id, json!({ "tools": tool_list }))
        }
        Err(e) => json_rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            id,
            INTERNAL_ERROR,
            e.to_string(),
        ),
    }
}

async fn handle_tools_call(
    module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
    state: &GatewayState,
) -> axum::response::Response {
    let params = match params {
        Some(p) => p,
        None => {
            return json_rpc_error(
                StatusCode::OK,
                id,
                INVALID_PARAMS,
                "params are required for tools/call",
            );
        }
    };

    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return json_rpc_error(
                StatusCode::OK,
                id,
                INVALID_PARAMS,
                "params.name is required for tools/call",
            );
        }
    };

    let args = params
        .get("arguments")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());

    let mut reg = state.registry.write().await;
    let module = match reg.get_mut(module_name) {
        Some(m) => m,
        None => {
            return module_not_found(id, module_name);
        }
    };

    // Pre-invocation credit check
    if let Some(ref guard) = state.credit_guard {
        if let Err(e) = guard.has_credits(module_name).await {
            drop(reg);
            return json_rpc_error(
                StatusCode::OK,
                id,
                -32000,
                e.to_string(),
            );
        }
    }

    match module.invoke_tool(&tool_name, &args).await {
        Ok(result) => {
            let metrics = module.last_invocation_metrics();
            drop(reg);

            if let Some(ref usage) = state.usage {
                tokio::spawn({
                    let usage = Arc::clone(usage);
                    let name = module_name.to_string();
                    async move {
                        usage.record_invocation(
                            &name,
                            "latest",
                            metrics.as_ref().and_then(|m| m.input_tokens.map(|t| t as i32)),
                            metrics.as_ref().and_then(|m| m.output_tokens.map(|t| t as i32)),
                            metrics.as_ref().map(|m| m.fuel_consumed),
                            metrics.as_ref().map(|m| m.execution_ms),
                        ).await;
                    }
                });
            }

            json_rpc_ok(
                id,
                json!({ "content": [{ "type": "text", "text": result }] }),
            )
        }
        Err(e) => json_rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            id,
            INTERNAL_ERROR,
            e.to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Handler: GET /mcp/{module}/sse
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_sse(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    // Verify the module exists.
    {
        let reg = state.registry.read().await;
        if reg.get(&module_name).is_none() {
            return module_not_found_json(&module_name);
        }
    }

    // Return an SSE stream that immediately sends an endpoint event and then
    // stays open.  Clients use this URL to discover the POST endpoint.
    let body = format!("event: endpoint\ndata: /mcp/{}\n\n", module_name);

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());

    (StatusCode::OK, headers, body).into_response()
}

// ---------------------------------------------------------------------------
// Helper: serialize a ToolDefinition to the MCP JSON shape
// ---------------------------------------------------------------------------

fn tool_to_json(tool: &ToolDefinition) -> Value {
    // parameters_schema is already a complete JSON Schema object
    // (e.g. {"type":"object","properties":{...},"required":[...]})
    let input_schema = serde_json::from_str::<Value>(&tool.parameters_schema)
        .unwrap_or(json!({"type": "object", "properties": {}}));

    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": input_schema
    })
}
