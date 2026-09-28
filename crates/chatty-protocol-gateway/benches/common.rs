//! Shared setup for the S7 performance benches (AGE-603 / plugin evaluation
//! plan §3 S7). Not a real module of the crate — `#[path]`-included by each
//! bench binary — so some helpers are unused in any one bench.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_wasm_runtime::test_support::{FakeLlm, FakeResponse, fixture_path};
use chatty_wasm_runtime::{Engine, LlmProvider, ModuleManifest, ResourceLimits, WasmModule};
use tokio::sync::RwLock;

/// Directory a fixture's `module.toml` lives in (what the registry loads).
pub fn module_dir(fixture: &str) -> PathBuf {
    fixture_path(fixture)
        .parent()
        .expect("a fixture has a directory")
        .to_path_buf()
}

/// A fresh `Engine`, configured exactly as `WasmModule` configures its own
/// (default resource limits only affect the store, not the engine).
///
/// Post-PL-H1 (AGE-604, #941), fuel is reset before every call rather than
/// set once at instantiate, so a long-lived module surviving many criterion
/// iterations no longer runs out of a lifetime fuel budget — plain
/// `ResourceLimits::default()` is fine here now (pre-H1 runs needed a huge
/// fuel override to work around that defect; see git history on this file).
pub fn engine() -> Engine {
    WasmModule::build_engine(&ResourceLimits::default()).expect("engine builds")
}

/// An `LlmProvider` that answers instantly with `reply`, for plugins whose
/// tools call the host (F1/F2 do not apply to a provider that never sleeps
/// and never loops). Scripted with enough repeats for a whole
/// benchmark's iterations: `FakeLlm` errors past the end of its script.
pub fn fake_llm(reply: &str) -> Arc<FakeLlm> {
    fake_llm_many(reply, 1_000_000)
}

/// As [`fake_llm`], with an explicit repeat count.
pub fn fake_llm_many(reply: &str, n: usize) -> Arc<FakeLlm> {
    Arc::new(FakeLlm::new(std::iter::repeat_n(
        FakeResponse::Text(reply.to_string()),
        n,
    )))
}

/// Load `fixture` directly against `engine`, bypassing the registry and
/// gateway entirely — the "direct call" baseline every protocol is compared
/// against.
pub fn load_module(engine: &Engine, fixture: &str, llm: Arc<dyn LlmProvider>) -> WasmModule {
    let path = fixture_path(fixture);
    WasmModule::from_file(
        engine,
        &path,
        ModuleManifest::new(fixture),
        llm,
        ResourceLimits::default(),
    )
    .unwrap_or_else(|e| panic!("{fixture} loads: {e:#}"))
}

/// Start a real gateway (MCP + A2A routes, no participant socket)
/// serving `fixtures` exactly as shipped, with one `FakeLlm` behind every
/// module's `llm::complete`. Returns the base URL and the fake provider.
pub async fn start_gateway(fixtures: &[&str]) -> (String, Arc<FakeLlm>) {
    let llm = fake_llm("bench-reply");
    let provider: Arc<dyn LlmProvider> = llm.clone();
    let mut registry = ModuleRegistry::new(provider, ResourceLimits::default()).unwrap();
    for fixture in fixtures {
        registry
            .load(module_dir(fixture))
            .unwrap_or_else(|e| panic!("{fixture} loads: {e:#}"));
    }
    let gateway = ProtocolGateway::new(Arc::new(RwLock::new(registry)), 0);
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base = format!("http://{}", tcp.local_addr().unwrap());
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });
    (base, llm)
}

/// A short-lived multi-thread runtime for `bench_function`-style (non-async)
/// criterion groups that still need to drive a `Future` to completion.
pub fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}
