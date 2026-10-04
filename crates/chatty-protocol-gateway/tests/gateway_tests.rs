//! Integration tests for `chatty-protocol-gateway`.
//!
//! These tests use `axum::Router` directly (without binding a real socket) so
//! they run quickly and don't need a free port.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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

/// The gateway's router, with its launch token added to every request, as
/// a caller holding it sends them.
fn gateway_router() -> axum::Router {
    let gateway = ProtocolGateway::new(empty_registry());
    let bearer = format!("Bearer {}", gateway.token().as_str());
    gateway
        .build_router()
        .layer(tower::util::MapRequestLayer::new(
            move |mut request: Request<Body>| {
                request
                    .headers_mut()
                    .insert(header::AUTHORIZATION, bearer.parse().unwrap());
                request
            },
        ))
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
// OpenAI routes: gone with 0.2.0's `chat` export (PL-U3)
// ---------------------------------------------------------------------------

/// A `chatty:plugin@0.4.0` plugin has tools and no loop, so there is nothing
/// for an OpenAI chat completion to call: neither route exists any more.
#[tokio::test]
async fn openai_routes_are_gone() {
    let request = serde_json::json!({
        "model": "module:echo",
        "messages": [{"role": "user", "content": "hi"}]
    });
    for path in ["/v1/chat/completions", "/v1/echo/chat/completions"] {
        let (status, _) = post_json(gateway_router(), path, request.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
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
async fn aggregated_agent_card_is_marked_as_the_gateway() {
    let (status, body) = get_json(gateway_router(), "/.well-known/agent.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gateway"], true);
}

// ---------------------------------------------------------------------------
// ProtocolGateway lifecycle test
// ---------------------------------------------------------------------------

/// Every route the router has, and a path it does not, by method.
const EVERY_ROUTE: &[(&str, &str)] = &[
    ("GET", "/"),
    ("GET", "/.well-known/agent.json"),
    ("POST", "/mcp/echo"),
    ("GET", "/mcp/echo/sse"),
    ("POST", "/mcp/echo/sse?sessionId=abc"),
    ("GET", "/a2a/local-agent/.well-known/agent.json"),
    ("POST", "/a2a/local-agent"),
    ("POST", "/v1/chat/completions"),
    ("GET", "/no-such-route"),
];

async fn status_with(
    router: &axum::Router,
    method: &str,
    path: &str,
    auth: Option<&str>,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let body = if method == "POST" {
        Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
    } else {
        Body::empty()
    };
    router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
        .status()
}

/// EN-0d (ADR-0021 § 4): no route answers a caller without this launch's
/// token — not the directory, not a module, not an agent, not a 404.
#[tokio::test]
async fn gateway_rejects_missing_token_on_every_route() {
    let gateway = ProtocolGateway::new(empty_registry());
    let router = gateway.build_router();
    let token = gateway.token().as_str().to_string();
    let other = ProtocolGateway::new(empty_registry());

    for (method, path) in EVERY_ROUTE {
        for auth in [
            None,
            Some(String::new()),
            Some(token.clone()),
            Some(format!("Basic {token}")),
            Some(format!("Bearer {}", &token[..token.len() - 1])),
            Some(format!("Bearer {}", other.token().as_str())),
        ] {
            let status = status_with(&router, method, path, auth.as_deref()).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} with {:?} must be refused",
                auth.map(|a| a.replace(&token, "<token>"))
            );
        }
        let status = status_with(&router, method, path, Some(&format!("Bearer {token}"))).await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} with the token must pass the check"
        );
    }
}

// ---------------------------------------------------------------------------
// ProtocolGateway lifecycle test
// ---------------------------------------------------------------------------

/// One HTTP/1.1 request over a Unix socket; the response's status code.
#[cfg(unix)]
async fn status_over_socket(socket: &std::path::Path, path: &str, token: Option<&str>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("not an HTTP response: {response:?}"))
}

#[cfg(unix)]
#[tokio::test]
async fn gateway_start_and_shutdown() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("run");
    let mut gateway = ProtocolGateway::new(empty_registry()).with_runtime_dir(&dir);
    gateway.start().await.expect("gateway should start");

    let socket = gateway
        .socket_path()
        .expect("a started gateway has a socket")
        .to_path_buf();
    let token_file = gateway
        .token_path()
        .expect("and a token file")
        .to_path_buf();
    assert_eq!(socket, dir.join("gateway.sock"));
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700, "the directory is owner-only");
    assert_eq!(mode(&token_file), 0o600, "the token file is owner-only");
    let token = std::fs::read_to_string(&token_file).unwrap();
    assert_eq!(token, gateway.token().as_str());

    assert_eq!(status_over_socket(&socket, "/", None).await, 401);
    assert_eq!(status_over_socket(&socket, "/", Some(&token)).await, 200);

    // A second gateway does not take a live socket from the first.
    let mut second = ProtocolGateway::new(empty_registry()).with_runtime_dir(&dir);
    assert!(
        second.start().await.is_err(),
        "a live socket is not replaced"
    );
    assert_eq!(status_over_socket(&socket, "/", Some(&token)).await, 200);

    gateway.shutdown();
    assert!(!socket.exists(), "shutdown removes the socket");
    assert!(!token_file.exists(), "and the token file");
}
