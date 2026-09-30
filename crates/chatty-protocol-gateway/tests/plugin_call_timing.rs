//! PL-U2 (AGE-616) item 10: one echo `reverse` call in-process, as a spec's
//! plugin tool (`chatty_core::tools::plugin_tool`), against the same call on
//! the path a module tool took before PL-U2 — an rmcp client over streamable
//! HTTP to this gateway's `/mcp/{module}`, under the module's lock, into the
//! same WASM export.
//!
//! A measurement, not a gate: it asserts both paths answer correctly and
//! prints per-call medians for the PR; wall-clock bounds on a shared CI box
//! would only flake. Run it alone for numbers:
//! `cargo test -p chatty-protocol-gateway --test plugin_call_timing -- --nocapture`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_core::agent_spec::PluginSpec;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_core::tools::plugin_tool::{PluginApprovals, PluginHost, PluginTool, load_plugins};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::test_support::{FakeLlm, fixture_path};
use chatty_wasm_runtime::{LlmProvider, ResourceLimits, ToolCallRequest, WasmModule};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::json;
use tokio::sync::RwLock;

const CALLS: usize = 200;
const WARM_UP: usize = 10;

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
async fn in_process_plugin_call_vs_the_mcp_path() {
    let echo_dir = fixture_path("echo")
        .parent()
        .expect("a fixture has a directory")
        .to_path_buf();

    // In-process: the plugin tool an agent built from a spec registers.
    let host = PluginHost {
        module_roots: vec![echo_dir.parent().map(Path::to_path_buf).unwrap()],
        ..PluginHost::default()
    };
    let calling = ModelConfig::new(
        "m".to_string(),
        "m".to_string(),
        ProviderType::Ollama,
        "m".to_string(),
    );
    let plugins = load_plugins(
        &[PluginSpec {
            module: "echo".to_string(),
            ..PluginSpec::default()
        }],
        &host,
        &calling,
    )
    .await
    .expect("echo loads as a plugin");
    let reverse = PluginTool::all(
        &plugins[0],
        &PluginApprovals {
            pending: None,
            mode: Default::default(),
        },
    )
    .into_iter()
    .find(|tool| tool.definition().tool == "reverse")
    .expect("reverse is a plugin tool");

    let mut in_process = Vec::with_capacity(CALLS);
    for i in 0..WARM_UP + CALLS {
        let started = Instant::now();
        let out = reverse
            .call(json!({ "input": "hello" }))
            .await
            .expect("reverse runs");
        if i >= WARM_UP {
            in_process.push(started.elapsed());
        }
        assert_eq!(out, "olleh");
    }

    // The floor: `WasmModule::invoke_tool` itself (PL-H1's per-call path:
    // fuel, epoch deadline, `spawn_blocking`), no tool wrapper around it.
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();
    let mut module = WasmModule::from_file(
        &engine,
        &fixture_path("echo"),
        chatty_wasm_runtime::ModuleManifest::new("echo"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    )
    .expect("echo loads");
    let mut direct = Vec::with_capacity(CALLS);
    for i in 0..WARM_UP + CALLS {
        let started = Instant::now();
        let out = module
            .invoke_tool(ToolCallRequest {
                name: "reverse".to_string(),
                arguments_json: r#"{"input":"hello"}"#.to_string(),
                call_id: format!("call-{i}"),
                caller: None,
            })
            .await
            .expect("reverse runs");
        if i >= WARM_UP {
            direct.push(started.elapsed());
        }
        assert_eq!(out.content, "olleh");
    }

    // The MCP path: the gateway serving the same module, and rmcp.
    let provider: Arc<dyn LlmProvider> = Arc::new(FakeLlm::default());
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    registry
        .load(&echo_dir)
        .expect("echo loads into the registry");
    let gateway = ProtocolGateway::new(Arc::new(RwLock::new(registry)));
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base = format!("http://{}", tcp.local_addr().unwrap());
    let router = with_launch_token(&gateway);
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });
    let transport = StreamableHttpClientTransport::from_uri(format!("{base}/mcp/echo"));
    let client = ().serve(transport).await.expect("the MCP session opens");

    let mut over_mcp = Vec::with_capacity(CALLS);
    for i in 0..WARM_UP + CALLS {
        let serde_json::Value::Object(arguments) = json!({ "input": "hello" }) else {
            unreachable!()
        };
        let started = Instant::now();
        let result = client
            .call_tool(CallToolRequestParams::new("reverse").with_arguments(arguments))
            .await
            .expect("tools/call answers");
        if i >= WARM_UP {
            over_mcp.push(started.elapsed());
        }
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone());
        assert_eq!(text.as_deref(), Some("olleh"));
    }
    client.cancel().await.ok();

    let (direct, in_process, over_mcp) = (median(direct), median(in_process), median(over_mcp));
    eprintln!(
        "PL-U2 per-call median over {CALLS} calls: WasmModule::invoke_tool {direct:?}, \
         in-process plugin tool {in_process:?}, MCP path {over_mcp:?} ({:.1}x the plugin tool)",
        over_mcp.as_secs_f64() / in_process.as_secs_f64()
    );
}

/// The gateway's router with its launch token added to every request: this
/// test's own listener stands in for a caller that holds the token (EN-0d).
fn with_launch_token(gateway: &ProtocolGateway) -> axum::Router {
    let bearer: axum::http::HeaderValue = format!("Bearer {}", gateway.token().as_str())
        .parse()
        .expect("a token is a valid header value");
    gateway
        .build_router()
        .layer(tower::util::MapRequestLayer::new(
            move |mut request: axum::extract::Request| {
                request
                    .headers_mut()
                    .insert(axum::http::header::AUTHORIZATION, bearer.clone());
                request
            },
        ))
}
