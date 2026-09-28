//! End-to-end integration tests for the `echo` reference plugin
//! (`chatty:plugin@0.3.0`).
//!
//! These tests exercise every layer of the chatty plugin pipeline:
//!
//! * Steps 2–6: direct module API (registry → WasmModule)
//! * Steps 9–12: HTTP protocol gateway (axum Router via tower oneshot): the
//!   plugin's tools over MCP, and no agent routes for it (PL-U3)
//!
//! # Prerequisites
//!
//! The echo WASM must be built and staged before running these tests:
//!
//! ```sh
//! scripts/build-wasm-fixtures.sh
//! ```
//!
//! The file is build output, so a fresh checkout does not have it. Every test
//! here looks it up first and, if it is missing, fails immediately (before any
//! WASM is loaded) naming the path and the script. Set `ECHO_PLUGIN_WASM` to use
//! a plugin built elsewhere.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::test_support::fixture_path;
use chatty_wasm_runtime::{
    CompletionResponse, LlmProvider, Message, ResourceLimits, ToolCallRequest,
};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Mock LLM provider
// ---------------------------------------------------------------------------

/// The echo plugin requests no capability and never calls `llm::complete`.
struct MockLlmProvider;

impl LlmProvider for MockLlmProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("the echo plugin never calls llm::complete".to_string())
    }
}

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

/// Return the directory holding the echo plugin's `module.toml` and `.wasm`.
///
/// Checks `ECHO_PLUGIN_WASM` first; falls back to the fixture staged by
/// `scripts/build-wasm-fixtures.sh`, which panics naming that script when it
/// has not been built.
fn find_echo_dir() -> Result<PathBuf, String> {
    // Allow explicit override for CI or unusual layouts.
    if let Ok(wasm) = std::env::var("ECHO_PLUGIN_WASM") {
        let wasm_path = PathBuf::from(&wasm);
        // The parent directory must contain a module.toml so the registry can
        // discover and load the module correctly.
        let parent = wasm_path
            .parent()
            .filter(|p| p.join("module.toml").exists());
        return match parent {
            Some(parent) if wasm_path.exists() => Ok(parent.to_path_buf()),
            _ => Err(format!(
                "ECHO_PLUGIN_WASM={wasm} does not point at a built echo.wasm \
                 with a module.toml beside it"
            )),
        };
    }

    let wasm = fixture_path("echo");
    Ok(wasm
        .parent()
        .expect("a staged fixture lives in its own directory")
        .to_path_buf())
}

/// Build a registry with only the echo plugin loaded (no RwLock wrapper).
fn registry_with_echo(dir: &PathBuf) -> ModuleRegistry {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    registry.load(dir).expect("failed to load echo plugin");
    registry
}

/// Build an axum Router backed by a registry containing the echo plugin.
fn gateway_router_with_echo(dir: &PathBuf) -> axum::Router {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    registry.load(dir).expect("failed to load echo plugin");
    let registry = Arc::new(RwLock::new(registry));
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
// Helper macro: fail fast, before any WASM is loaded, when the fixture is
// missing. Do not turn this into a skip: a skipped test is reported as `ok`
// and its message is captured, so a fresh checkout would look green while
// testing nothing.
// ---------------------------------------------------------------------------

macro_rules! require_echo {
    ($dir:ident) => {
        let $dir = match find_echo_dir() {
            Ok(dir) => dir,
            Err(msg) => panic!("{msg}"),
        };
    };
}

/// A tool call on the echo plugin with `{"input": input}`.
fn call(tool: &str, input: &str) -> ToolCallRequest {
    ToolCallRequest {
        name: tool.to_string(),
        arguments_json: json!({ "input": input }).to_string(),
        call_id: "e2e".to_string(),
        caller: None,
    }
}

// ---------------------------------------------------------------------------
// Step 2 — Module registry discovers and loads echo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_02_echo_is_discovered_and_loaded() {
    require_echo!(module_dir);

    // The staging root is the parent of the echo directory.
    let modules_root = module_dir.parent().unwrap();
    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    let report = registry.scan_directory(modules_root).unwrap();
    assert!(
        report.loaded_names().contains(&"echo"),
        "echo not discovered; found: {report:?}"
    );
    assert!(registry.get("echo").is_some(), "echo not in registry after scan");
}

// ---------------------------------------------------------------------------
// Step 3 — list_tools() returns 3 tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_03_list_tools_returns_three_tools() {
    require_echo!(module_dir);

    let registry = registry_with_echo(&module_dir);
    let module = registry.get("echo").unwrap();
    let mut module = module.lock().await;

    let tools = module.list_tools().expect("list_tools failed");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "reverse", "count_words"], "{tools:?}");
}

// ---------------------------------------------------------------------------
// Step 4 — invoke_tool(echo, {"input":"hello"}) returns "hello"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_04_invoke_echo_returns_input_unchanged() {
    require_echo!(module_dir);

    let registry = registry_with_echo(&module_dir);
    let module = registry.get("echo").unwrap();
    let mut module = module.lock().await;

    let result = module.invoke_tool(call("echo", "hello")).await.unwrap();
    assert_eq!(result.content, "hello");
    assert!(result.usage.is_none(), "echo spends no model usage");
}

// ---------------------------------------------------------------------------
// Step 5 — invoke_tool(reverse, {"input":"hello"}) returns "olleh"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_05_invoke_reverse_returns_reversed() {
    require_echo!(module_dir);

    let registry = registry_with_echo(&module_dir);
    let module = registry.get("echo").unwrap();
    let mut module = module.lock().await;

    let result = module.invoke_tool(call("reverse", "hello")).await.unwrap();
    assert_eq!(result.content, "olleh");
}

// ---------------------------------------------------------------------------
// Step 6 — metadata() names the plugin and requests no capability. Replaces
// 0.2.0's `chat` and `get-agent-card` steps: a plugin has neither (PL-U3).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_06_metadata_names_the_plugin_and_requests_nothing() {
    require_echo!(module_dir);

    let registry = registry_with_echo(&module_dir);
    let module = registry.get("echo").unwrap();
    let mut module = module.lock().await;

    let metadata = module.metadata().expect("metadata failed");
    assert_eq!(metadata.name, "echo");
    assert_eq!(metadata.version, "0.2.0");
    assert!(metadata.requested_capabilities.is_empty(), "{metadata:?}");
    assert!(metadata.config_keys.is_empty(), "{metadata:?}");
}

// ---------------------------------------------------------------------------
// Step 9 — GET /.well-known/agent.json does not list echo: a plugin is never
// an agent (PL-D1 option B)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_09_well_known_agent_json_does_not_list_the_plugin() {
    require_echo!(module_dir);

    let router = gateway_router_with_echo(&module_dir);
    let (status, body) = get_json(router, "/.well-known/agent.json").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let agents = body["agents"].as_array().expect("agents should be an array");
    assert!(agents.is_empty(), "a plugin must not appear as an agent: {body}");
}

// ---------------------------------------------------------------------------
// Step 10 — POST /mcp/echo tools/list returns JSON-RPC with 3 tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_10_mcp_tools_list_returns_three_tools() {
    require_echo!(module_dir);

    let router = gateway_router_with_echo(&module_dir);
    let (status, body) = post_json(
        router,
        "/mcp/echo",
        json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["jsonrpc"], "2.0");
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    assert_eq!(tools.len(), 3, "expected 3 tools; body: {body}");
}

// ---------------------------------------------------------------------------
// Step 11 — POST /mcp/echo tools/call runs the tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_11_mcp_tools_call_runs_the_tool() {
    require_echo!(module_dir);

    let router = gateway_router_with_echo(&module_dir);
    let (status, body) = post_json(
        router,
        "/mcp/echo",
        json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 2,
            "params": {"name": "count_words", "arguments": {"input": "one two three"}}
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["result"]["content"][0]["text"], "3", "body: {body}");
}

// ---------------------------------------------------------------------------
// Step 12 — the plugin has no OpenAI or A2A route (PL-U3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_12_plugin_has_no_openai_or_a2a_route() {
    require_echo!(module_dir);

    let (status, body) = post_json(
        gateway_router_with_echo(&module_dir),
        "/a2a/echo",
        json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "id": 1,
            "params": {"message": {"parts": [{"type": "text", "text": "hello a2a"}]}}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");

    let (status, _) = get_json(
        gateway_router_with_echo(&module_dir),
        "/a2a/echo/.well-known/agent.json",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = post_json(
        gateway_router_with_echo(&module_dir),
        "/v1/echo/chat/completions",
        json!({"model": "echo", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
