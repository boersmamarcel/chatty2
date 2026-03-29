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
use tokio::sync::RwLock;

use chatty_module_registry::ModuleRegistry;

use super::jsonrpc::{JsonRpcRequest, JsonRpcResponse};

// ---------------------------------------------------------------------------
// Handler: POST /mcp/{module}
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_jsonrpc(
    Path(module_name): Path<String>,
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
    Json(body): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if body.jsonrpc != "2.0" {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(JsonRpcResponse::err(
                    body.id,
                    -32600,
                    "Invalid Request: jsonrpc must be \"2.0\"",
                ))
                .unwrap_or_default(),
            ),
        )
            .into_response();
    }

    match body.method.as_str() {
        "tools/list" => handle_tools_list(&module_name, body.id, registry).await,
        "tools/call" => handle_tools_call(&module_name, body.id, body.params, registry).await,
        method => (
            StatusCode::OK,
            Json(
                serde_json::to_value(JsonRpcResponse::err(
                    body.id,
                    -32601,
                    format!("Method not found: {}", method),
                ))
                .unwrap_or_default(),
            ),
        )
            .into_response(),
    }
}

async fn handle_tools_list(
    module_name: &str,
    id: Option<Value>,
    registry: Arc<RwLock<ModuleRegistry>>,
) -> axum::response::Response {
    let mut reg = registry.write().await;
    let module = match reg.get_mut(module_name) {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(
                    serde_json::to_value(JsonRpcResponse::err(
                        id,
                        -32602,
                        format!("module '{}' not found", module_name),
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response();
        }
    };

    match module.list_tools() {
        Ok(tools) => {
            let tool_list: Vec<Value> = tools.iter().map(tool_to_json).collect();
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(JsonRpcResponse::ok(id, json!({ "tools": tool_list })))
                        .unwrap_or_default(),
                ),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::to_value(JsonRpcResponse::err(id, -32603, e.to_string()))
                    .unwrap_or_default(),
            ),
        )
            .into_response(),
    }
}

async fn handle_tools_call(
    module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
    registry: Arc<RwLock<ModuleRegistry>>,
) -> axum::response::Response {
    let params = match params {
        Some(p) => p,
        None => {
            return (
                StatusCode::OK,
                Json(
                    serde_json::to_value(JsonRpcResponse::err(
                        id,
                        -32602,
                        "params are required for tools/call",
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response();
        }
    };

    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return (
                StatusCode::OK,
                Json(
                    serde_json::to_value(JsonRpcResponse::err(
                        id,
                        -32602,
                        "params.name is required for tools/call",
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response();
        }
    };

    let args = params
        .get("arguments")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());

    let mut reg = registry.write().await;
    let module = match reg.get_mut(module_name) {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(
                    serde_json::to_value(JsonRpcResponse::err(
                        id,
                        -32602,
                        format!("module '{}' not found", module_name),
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response();
        }
    };

    match module.invoke_tool(&tool_name, &args).await {
        Ok(result) => {
            let result_value: Value =
                serde_json::from_str(&result).unwrap_or(Value::String(result));
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(JsonRpcResponse::ok(
                        id,
                        json!({ "content": [{ "type": "text", "text": result_value }] }),
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::to_value(JsonRpcResponse::err(id, -32603, e.to_string()))
                    .unwrap_or_default(),
            ),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Handler: GET /mcp/{module}/sse
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_sse(
    Path(module_name): Path<String>,
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
) -> impl IntoResponse {
    // Verify the module exists.
    {
        let reg = registry.read().await;
        if reg.get(&module_name).is_none() {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("module '{}' not found", module_name) })),
            )
                .into_response();
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
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": parse_params(&tool.parameters_schema),
        }
    })
}

fn parse_params(schema: &str) -> Value {
    serde_json::from_str(schema).unwrap_or(json!({}))
}
