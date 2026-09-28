//! MCP (Model Context Protocol) JSON-RPC handlers.
//!
//! Routes:
//! - `POST /mcp/{module}` — MCP JSON-RPC over streamable HTTP: each request
//!   is answered in its own response
//! - `GET  /mcp/{module}/sse` — the legacy HTTP+SSE transport (MCP
//!   2024-11-05): opens an event stream whose first event, `endpoint`, names
//!   the URL to POST messages to; answers come back on the stream as
//!   `message` events
//! - `POST /mcp/{module}/sse?sessionId=…` — a client message on that stream
//!
//! Both transports dispatch through the same [`dispatch`], so a module
//! answers the same over either.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use chatty_wasm_runtime::{ToolCallRequest, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::gateway::{GatewayState, MAX_REQUEST_BYTES};

use super::jsonrpc::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND,
    json_rpc_error, json_rpc_ok, module_not_found, module_not_found_json,
};
use super::module_call;

// ---------------------------------------------------------------------------
// Handler: POST /mcp/{module}
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_jsonrpc(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
    Json(body): Json<JsonRpcRequest>,
) -> Response {
    dispatch(&module_name, body, &state).await
}

/// Answer one JSON-RPC message for `module_name`.
async fn dispatch(module_name: &str, body: JsonRpcRequest, state: &GatewayState) -> Response {
    if body.jsonrpc != "2.0" {
        return json_rpc_error(
            StatusCode::BAD_REQUEST,
            body.id,
            INVALID_REQUEST,
            "Invalid Request: jsonrpc must be \"2.0\"",
        );
    }

    let method = body.method.as_str();
    if matches!(method, "initialize" | "tools/list" | "tools/call") {
        let Some(module) = module_call::mcp_module(state, module_name).await else {
            return module_not_found(body.id, module_name);
        };
        return match method {
            "initialize" => handle_initialize(module_name, body.id),
            "tools/list" => handle_tools_list(module, body.id).await,
            _ => handle_tools_call(module_name, module, body.id, body.params, state).await,
        };
    }

    match method {
        // MCP lifecycle
        method if method.starts_with("notifications/") || method == "initialized" => {
            (StatusCode::ACCEPTED, "").into_response()
        }
        "ping" => json_rpc_ok(body.id, json!({})),
        method => json_rpc_error(
            StatusCode::OK,
            body.id,
            METHOD_NOT_FOUND,
            format!("Method not found: {}", method),
        ),
    }
}

fn handle_initialize(module_name: &str, id: Option<Value>) -> Response {
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
    module: chatty_module_registry::ModuleHandle,
    id: Option<Value>,
) -> Response {
    match module_call::blocking(module, |m| m.list_tools()).await {
        Ok(tools) => {
            let tool_list: Vec<Value> = tools.iter().map(tool_to_json).collect();
            json_rpc_ok(id, json!({ "tools": tool_list }))
        }
        Err(e) => json_rpc_error(
            module_call::failure_status(&e),
            id,
            INTERNAL_ERROR,
            format!("{e:#}"),
        ),
    }
}

async fn handle_tools_call(
    module_name: &str,
    module: chatty_module_registry::ModuleHandle,
    id: Option<Value>,
    params: Option<Value>,
    state: &GatewayState,
) -> Response {
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

    // The WIT contract: `arguments-json` is the tool's arguments object,
    // JSON-encoded once — exactly what the tool's `inputSchema` describes.
    let call = ToolCallRequest {
        name: tool_name,
        arguments_json: params
            .get("arguments")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string()),
        call_id: crate::gateway::new_id(),
        caller: None,
    };

    // Pre-invocation credit check for paid modules only
    if let Err(e) = module_call::check_credits(state, module_name).await {
        return json_rpc_error(StatusCode::OK, id, -32000, e);
    }
    if let Err(e) = module_call::check_usage_reporting(state, module_name) {
        return json_rpc_error(StatusCode::OK, id, -32000, e);
    }

    let mut module = module.lock().await;
    let result = module.invoke_tool(call).await;
    let metrics = module.last_invocation_metrics();
    drop(module);

    match result {
        Ok(result) => {
            module_call::record_usage(state, module_name, metrics);
            json_rpc_ok(
                id,
                json!({ "content": [{ "type": "text", "text": result.content }] }),
            )
        }
        Err(e) => json_rpc_error(
            module_call::failure_status(&e),
            id,
            INTERNAL_ERROR,
            format!("{e:#}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// The HTTP+SSE transport: GET and POST /mcp/{module}/sse
// ---------------------------------------------------------------------------

/// The open SSE streams: session id → the module it serves and the sender
/// its `message` events go out on. A session is removed when its stream is
/// dropped (the client went away).
#[derive(Clone, Default)]
pub(crate) struct SseSessions(Arc<Mutex<HashMap<String, SseSession>>>);

struct SseSession {
    module: String,
    events: mpsc::UnboundedSender<String>,
}

impl SseSessions {
    fn open(&self, module: &str) -> (String, mpsc::UnboundedReceiver<String>) {
        let (events, rx) = mpsc::unbounded_channel();
        let id = crate::gateway::new_id();
        self.lock().insert(
            id.clone(),
            SseSession {
                module: module.to_string(),
                events,
            },
        );
        (id, rx)
    }

    /// The sender of session `id`, if it is open and serves `module`.
    fn sender(&self, id: &str, module: &str) -> Option<mpsc::UnboundedSender<String>> {
        self.lock()
            .get(id)
            .filter(|session| session.module == module)
            .map(|session| session.events.clone())
    }

    fn close(&self, id: &str) {
        self.lock().remove(id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SseSession>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Removes its session when the stream holding it is dropped.
struct CloseOnDrop {
    sessions: SseSessions,
    id: String,
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.sessions.close(&self.id);
    }
}

pub(crate) async fn mcp_sse(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
) -> Response {
    if module_call::mcp_module(&state, &module_name)
        .await
        .is_none()
    {
        return module_not_found_json(&module_name);
    }

    let (id, mut rx) = state.sse_sessions.open(&module_name);
    let endpoint = format!("/mcp/{module_name}/sse?sessionId={id}");
    let guard = CloseOnDrop {
        sessions: state.sse_sessions.clone(),
        id,
    };

    let stream = async_stream::stream! {
        let _guard = guard;
        yield Ok::<_, Infallible>(Event::default().event("endpoint").data(endpoint));
        while let Some(message) = rx.recv().await {
            yield Ok(Event::default().event("message").data(message));
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[derive(Deserialize)]
pub(crate) struct SessionQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// A client message on an open SSE session: dispatched like a streamable
/// HTTP request, its answer (if it has one) sent as a `message` event, and
/// the POST itself answered `202 Accepted`.
pub(crate) async fn mcp_sse_message(
    Path(module_name): Path<String>,
    Query(query): Query<SessionQuery>,
    State(state): State<GatewayState>,
    Json(body): Json<JsonRpcRequest>,
) -> Response {
    let Some(events) = state.sse_sessions.sender(&query.session_id, &module_name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no open SSE session '{}'", query.session_id) })),
        )
            .into_response();
    };

    let answer = dispatch(&module_name, body, &state).await;
    let bytes = match to_bytes(answer.into_body(), MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(module = %module_name, error = %e, "MCP SSE: unreadable answer");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // A notification has no answer (an empty 202 body).
    if !bytes.is_empty() {
        let _ = events.send(String::from_utf8_lossy(&bytes).into_owned());
    }
    StatusCode::ACCEPTED.into_response()
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
