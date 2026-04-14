//! Shared JSON-RPC 2.0 request/response envelope types used across protocols.

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Standard JSON-RPC 2.0 error codes
pub(crate) const INVALID_REQUEST: i32 = -32600;
pub(crate) const METHOD_NOT_FOUND: i32 = -32601;
pub(crate) const INVALID_PARAMS: i32 = -32602;
pub(crate) const INTERNAL_ERROR: i32 = -32603;

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub(crate) fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }

    /// Serialize into a JSON value, falling back to `Value::Null` on failure.
    fn into_json(self) -> Json<Value> {
        Json(serde_json::to_value(self).unwrap_or_default())
    }
}

/// Build a complete JSON-RPC error response with the given HTTP status.
pub(crate) fn json_rpc_error(
    status: StatusCode,
    id: Option<Value>,
    code: i32,
    msg: impl Into<String>,
) -> axum::response::Response {
    (status, JsonRpcResponse::err(id, code, msg).into_json()).into_response()
}

/// Build a complete JSON-RPC success response (HTTP 200).
pub(crate) fn json_rpc_ok(id: Option<Value>, result: Value) -> axum::response::Response {
    (StatusCode::OK, JsonRpcResponse::ok(id, result).into_json()).into_response()
}

/// Build a "module not found" JSON-RPC error response (HTTP 404).
pub(crate) fn module_not_found(id: Option<Value>, name: &str) -> axum::response::Response {
    json_rpc_error(
        StatusCode::NOT_FOUND,
        id,
        INVALID_PARAMS,
        format!("module '{}' not found", name),
    )
}

/// Build a "module not found" plain JSON error response (for non-JSON-RPC endpoints).
pub(crate) fn module_not_found_json(name: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": format!("module '{}' not found", name) })),
    )
        .into_response()
}
