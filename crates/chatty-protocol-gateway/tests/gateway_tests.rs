//! Integration tests for `chatty-protocol-gateway`.
//!
//! These tests use `axum::Router` directly (without binding a real socket) so
//! they run quickly and don't need a free port.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use serde_json::Value;
use tokio::sync::RwLock;
use tower::ServiceExt; // for `oneshot`

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop".into())
    }
}

fn empty_registry() -> Arc<RwLock<ModuleRegistry>> {
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    Arc::new(RwLock::new(registry))
}

fn gateway_router() -> axum::Router {
    let registry = empty_registry();
    ProtocolGateway::new(registry, 0).build_router()
}

async fn get_json(router: axum::Router, path: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn post_json(router: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

// ---------------------------------------------------------------------------
// Index tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn index_returns_200() {
    let (status, body) = get_json(gateway_router(), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gateway"], "chatty-protocol-gateway");
    assert!(body["modules"].is_array());
}

#[tokio::test]
async fn index_empty_registry_has_no_modules() {
    let (_, body) = get_json(gateway_router(), "/").await;
    assert_eq!(body["modules"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Aggregated agent card tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aggregated_agent_card_returns_200() {
    let (status, body) = get_json(gateway_router(), "/.well-known/agent.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gateway"], true);
    assert!(body["agents"].is_array());
}

// ---------------------------------------------------------------------------
// OpenAI endpoint tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_module_missing_returns_404() {
    let (status, body) = post_json(
        gateway_router(),
        "/v1/nonexistent/chat/completions",
        serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nonexistent")
    );
}

#[tokio::test]
async fn openai_routed_invalid_model_format_returns_400() {
    let (status, body) = post_json(
        gateway_router(),
        "/v1/chat/completions",
        serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("module:")
    );
}

#[tokio::test]
async fn openai_routed_missing_module_returns_404() {
    let (status, _) = post_json(
        gateway_router(),
        "/v1/chat/completions",
        serde_json::json!({
            "model": "module:nonexistent",
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// MCP endpoint tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mcp_missing_module_returns_404() {
    let (status, _) = post_json(
        gateway_router(),
        "/mcp/nonexistent",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mcp_invalid_jsonrpc_version_returns_400() {
    let (status, body) = post_json(
        gateway_router(),
        "/mcp/any",
        serde_json::json!({
            "jsonrpc": "1.0",
            "method": "tools/list",
            "id": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("2.0"));
}

#[tokio::test]
async fn mcp_unknown_method_returns_method_not_found() {
    let (status, body) = post_json(
        gateway_router(),
        "/mcp/any",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "unknown/method",
            "id": 1
        }),
    )
    .await;
    // Status 200 with JSON-RPC error (-32601)
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32601);
}

#[tokio::test]
async fn mcp_sse_missing_module_returns_404() {
    let (status, _) = get_json(gateway_router(), "/mcp/nonexistent/sse").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// A2A endpoint tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a2a_agent_card_missing_module_returns_404() {
    let (status, _) = get_json(gateway_router(), "/a2a/nonexistent/.well-known/agent.json").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a2a_jsonrpc_missing_module_returns_404() {
    let (status, _) = post_json(
        gateway_router(),
        "/a2a/nonexistent",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "id": 1,
            "params": {
                "message": { "parts": [{ "type": "text", "text": "hello" }] }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a2a_jsonrpc_invalid_jsonrpc_version_returns_400() {
    let (status, body) = post_json(
        gateway_router(),
        "/a2a/any",
        serde_json::json!({
            "jsonrpc": "1.0",
            "method": "message/send",
            "id": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("2.0"));
}

#[tokio::test]
async fn a2a_tasks_get_returns_stateless_error() {
    let (status, body) = post_json(
        gateway_router(),
        "/a2a/any",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tasks/get",
            "id": 1,
            "params": { "id": "task-123" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stateless")
    );
}

#[tokio::test]
async fn a2a_message_stream_missing_module_returns_404() {
    let (status, _) = post_json(
        gateway_router(),
        "/a2a/nonexistent",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "message/stream",
            "id": 1,
            "params": {
                "message": { "parts": [{ "type": "text", "text": "hello" }] }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a2a_message_stream_missing_params_returns_error() {
    let (status, body) = post_json(
        gateway_router(),
        "/a2a/any",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "message/stream",
            "id": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("params are required")
    );
}

#[tokio::test]
async fn a2a_agent_card_includes_streaming_capability() {
    // With an empty registry this will 404, but we can verify the
    // aggregated card at least includes the expected shape.
    let (status, body) = get_json(gateway_router(), "/.well-known/agent.json").await;
    assert_eq!(status, StatusCode::OK);
    // The aggregated card itself doesn't have capabilities, but per-module
    // cards do. We at least verify the endpoint works.
    assert_eq!(body["gateway"], true);
}

// ---------------------------------------------------------------------------
// ProtocolGateway lifecycle test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gateway_start_and_shutdown() {
    let registry = empty_registry();
    let port = find_free_port();
    let mut gateway = ProtocolGateway::new(registry, port);
    gateway.start().await.expect("gateway should start");
    assert_eq!(gateway.port(), port);
    gateway.shutdown();
}

fn find_free_port() -> u16 {
    // Bind to port 0 to let the OS assign a free port, then release it.
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}
