//! End-to-end integration tests for the echo-agent reference module.
//!
//! These tests exercise every layer of the chatty module pipeline:
//!
//! * Steps 2–7: direct module API (registry → WasmModule)
//! * Steps 8–12: HTTP protocol gateway (axum Router via tower oneshot)
//!
//! # Prerequisites
//!
//! The echo-agent WASM must be built and placed at
//! `modules/echo-agent/echo_agent.wasm` before running these tests:
//!
//! ```sh
//! make wasm-modules
//! ```
//!
//! The file is git-ignored, so a fresh checkout does not have it. Every test
//! here checks for it first and, if it is missing, fails immediately (before
//! any WASM is loaded) with the absolute path it looked at and the build
//! command. Set `ECHO_AGENT_WASM` to use a module built elsewhere.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::{
    ChatRequest, CompletionResponse, LlmProvider, Message, ResourceLimits, Role,
};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Mock LLM provider
// ---------------------------------------------------------------------------

/// Echoes the last user message with an "LLM: " prefix — used in tests that
/// exercise the `"use llm"` code path of the echo-agent.
struct MockLlmProvider;

impl LlmProvider for MockLlmProvider {
    fn complete(
        &self,
        _model: &str,
        messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let last = messages
            .iter()
            .rfind(|m| matches!(m.role, Role::User))
            .map(|m| m.content.as_str())
            .unwrap_or("");
        Ok(CompletionResponse {
            content: format!("LLM: {last}"),
            tool_calls: vec![],
            usage: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

/// Return the path to `modules/echo-agent/`, or an actionable error if the
/// compiled WASM is not there.
///
/// Checks `ECHO_AGENT_WASM` env var first; falls back to the canonical
/// workspace-relative path.
fn find_echo_agent_dir() -> Result<PathBuf, String> {
    // Allow explicit override for CI or unusual layouts.
    if let Ok(wasm) = std::env::var("ECHO_AGENT_WASM") {
        let wasm_path = PathBuf::from(&wasm);
        // The parent directory must contain a module.toml so the registry can
        // discover and load the module correctly.
        let parent = wasm_path
            .parent()
            .filter(|p| p.join("module.toml").exists());
        return match parent {
            Some(parent) if wasm_path.exists() => Ok(parent.to_path_buf()),
            _ => Err(format!(
                "ECHO_AGENT_WASM={wasm} does not point at a built echo_agent.wasm \
                 with a module.toml beside it"
            )),
        };
    }

    // Default: CARGO_MANIFEST_DIR → crates/chatty-protocol-gateway
    //          → parent → crates/ → parent → workspace root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent() // crates/chatty-protocol-gateway → crates/
        .and_then(|p| p.parent()) // crates/ → workspace root
        .map(|p| p.to_path_buf())
        .expect("CARGO_MANIFEST_DIR has a workspace root two levels up");

    let dir = workspace_root.join("modules").join("echo-agent");
    let wasm = dir.join("echo_agent.wasm");

    if dir.join("module.toml").exists() && wasm.exists() {
        Ok(dir)
    } else {
        Err(missing_wasm_message(&wasm, &workspace_root))
    }
}

/// The error a test fails with when the WASM fixture is missing: the absolute
/// path that was checked and the exact command that produces it.
fn missing_wasm_message(wasm: &Path, workspace_root: &Path) -> String {
    format!(
        "echo-agent WASM not found at {}\n\
         It is git-ignored and must be built once per checkout:\n  \
         cd {} && make wasm-modules\n\
         (or set ECHO_AGENT_WASM to a built echo_agent.wasm)",
        wasm.display(),
        workspace_root.display()
    )
}

/// Build a registry with only the echo-agent loaded (no RwLock wrapper).
fn registry_with_echo_agent(dir: &PathBuf) -> ModuleRegistry {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    registry.load(dir).expect("failed to load echo-agent");
    registry
}

/// Build an axum Router backed by a registry containing the echo-agent.
fn gateway_router_with_echo_agent(dir: &PathBuf) -> axum::Router {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    registry.load(dir).expect("failed to load echo-agent");
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

macro_rules! require_echo_agent {
    ($dir:ident) => {
        let $dir = match find_echo_agent_dir() {
            Ok(dir) => dir,
            Err(msg) => panic!("{msg}"),
        };
    };
}

#[test]
fn missing_wasm_message_names_path_and_build_command() {
    let root = Path::new("/some/checkout");
    let wasm = root.join("modules/echo-agent/echo_agent.wasm");
    let msg = missing_wasm_message(&wasm, root);
    assert!(
        msg.contains("/some/checkout/modules/echo-agent/echo_agent.wasm"),
        "message must name the absolute path checked: {msg}"
    );
    assert!(
        msg.contains("cd /some/checkout && make wasm-modules"),
        "message must give the exact build command: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Step 2 — Module registry discovers and loads echo-agent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_02_echo_agent_is_discovered_and_loaded() {
    require_echo_agent!(module_dir);

    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();

    // The modules/ root is the parent of the echo-agent directory.
    let modules_root = module_dir.parent().expect("modules root");
    let names = registry
        .scan_directory(modules_root)
        .expect("scan_directory failed");

    assert!(
        names.contains(&"echo-agent".to_string()),
        "echo-agent not discovered; found: {names:?}"
    );
    assert!(
        registry.get("echo-agent").is_some(),
        "echo-agent not in registry after scan"
    );
}

// ---------------------------------------------------------------------------
// Step 3 — list_tools() returns 3 tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_03_list_tools_returns_three_tools() {
    require_echo_agent!(module_dir);

    let mut registry = registry_with_echo_agent(&module_dir);
    let module = registry.get_mut("echo-agent").unwrap();

    let tools = module.list_tools().expect("list_tools failed");
    assert_eq!(tools.len(), 3, "expected 3 tools, got: {tools:?}");

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"), "missing 'echo' tool");
    assert!(names.contains(&"reverse"), "missing 'reverse' tool");
    assert!(names.contains(&"count_words"), "missing 'count_words' tool");
}

// ---------------------------------------------------------------------------
// Step 4 — invoke_tool("echo", "hello") returns "hello"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_04_invoke_echo_returns_input_unchanged() {
    require_echo_agent!(module_dir);

    let mut registry = registry_with_echo_agent(&module_dir);
    let module = registry.get_mut("echo-agent").unwrap();

    let result = module.invoke_tool("echo", "hello").await.unwrap();
    assert_eq!(result, "hello");
}

// ---------------------------------------------------------------------------
// Step 5 — invoke_tool("reverse", "hello") returns "olleh"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_05_invoke_reverse_returns_reversed() {
    require_echo_agent!(module_dir);

    let mut registry = registry_with_echo_agent(&module_dir);
    let module = registry.get_mut("echo-agent").unwrap();

    let result = module.invoke_tool("reverse", "hello").await.unwrap();
    assert_eq!(result, "olleh");
}

// ---------------------------------------------------------------------------
// Step 6 — chat(messages) returns echo response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_06_chat_returns_echo_response() {
    require_echo_agent!(module_dir);

    let mut registry = registry_with_echo_agent(&module_dir);
    let module = registry.get_mut("echo-agent").unwrap();

    let req = ChatRequest {
        messages: vec![Message {
            role: Role::User,
            content: "hello world".to_string(),
        }],
        conversation_id: "test-conv".to_string(),
    };

    let resp = module.chat(req).await.unwrap();
    assert_eq!(resp.content, "Echo: hello world");
    assert!(resp.tool_calls.is_empty());
}

// ---------------------------------------------------------------------------
// Step 7 — agent_card() has correct name and "echoing" skill
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_07_agent_card_has_correct_name_and_skills() {
    require_echo_agent!(module_dir);

    let mut registry = registry_with_echo_agent(&module_dir);
    let module = registry.get_mut("echo-agent").unwrap();

    let card = module.agent_card().expect("agent_card failed");
    assert_eq!(card.name, "echo-agent");
    assert!(
        card.skills.iter().any(|s| s.name == "echoing"),
        "expected 'echoing' skill; got: {:?}",
        card.skills
    );
}

// ---------------------------------------------------------------------------
// Step 8 — Protocol gateway starts (exercised implicitly by steps 9–12)
// ---------------------------------------------------------------------------

// The gateway is started implicitly for each HTTP test via `gateway_router_with_echo_agent`.

// ---------------------------------------------------------------------------
// Step 9 — GET /.well-known/agent.json lists echo-agent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_09_well_known_agent_json_lists_echo_agent() {
    require_echo_agent!(module_dir);

    let router = gateway_router_with_echo_agent(&module_dir);
    let (status, body) = get_json(router, "/.well-known/agent.json").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    let agents = body["agents"]
        .as_array()
        .expect("agents should be an array");
    let has_echo = agents.iter().any(|a| a["name"] == "echo-agent");
    assert!(has_echo, "echo-agent not found in agents: {body}");
}

// ---------------------------------------------------------------------------
// Step 10 — POST /mcp/echo-agent tools/list returns JSON-RPC with 3 tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_10_mcp_tools_list_returns_three_tools() {
    require_echo_agent!(module_dir);

    let router = gateway_router_with_echo_agent(&module_dir);
    let (status, body) = post_json(
        router,
        "/mcp/echo-agent",
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
// Step 11 — POST /v1/echo-agent/chat/completions returns OpenAI-format echo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_11_openai_chat_completions_returns_echo_response() {
    require_echo_agent!(module_dir);

    let router = gateway_router_with_echo_agent(&module_dir);
    let (status, body) = post_json(
        router,
        "/v1/echo-agent/chat/completions",
        json!({
            "model": "echo-agent",
            "messages": [{"role": "user", "content": "say something"}]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .expect("content should be a string");
    assert_eq!(content, "Echo: say something");
}

// ---------------------------------------------------------------------------
// Step 12 — POST /a2a/echo-agent message/send returns A2A response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn step_12_a2a_message_send_returns_response() {
    require_echo_agent!(module_dir);

    let router = gateway_router_with_echo_agent(&module_dir);
    let (status, body) = post_json(
        router,
        "/a2a/echo-agent",
        json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "id": 1,
            "params": {
                "message": {
                    "parts": [{"type": "text", "text": "hello a2a"}]
                }
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["jsonrpc"], "2.0", "body: {body}");
    assert!(
        body["result"].is_object(),
        "expected a result object; body: {body}"
    );
}
