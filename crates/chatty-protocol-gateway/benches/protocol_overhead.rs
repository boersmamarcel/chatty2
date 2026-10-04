//! S7 §2: per-call overhead of the gateway's MCP route vs a direct
//! in-process call (AGE-603, plugin evaluation plan §3 S7).
//!
//! A `chatty:plugin@0.4.0` plugin is served over MCP only (PL-U3): the
//! `chat`-shaped group (direct `chat`, OpenAI, A2A) went with the `chat`
//! export. What remains is the `invoke_tool`-shaped pairing: direct
//! `WasmModule::invoke_tool` against MCP `tools/call`, both on `echo`.
//!
//! Run: `CARGO_TARGET_DIR=<dir> cargo bench -p chatty-protocol-gateway --bench protocol_overhead`

#[path = "common.rs"]
mod common;

use chatty_wasm_runtime::{LlmProvider, ToolCallRequest};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use std::sync::Arc;

fn tool_shaped(c: &mut Criterion) {
    let rt = common::rt();
    let mut group = c.benchmark_group("tool_shaped");
    group.sample_size(30);

    // Direct: WasmModule::invoke_tool, no HTTP, no gateway.
    group.bench_function("direct_wasm_module_invoke_tool", |b| {
        let engine = common::engine();
        let llm: Arc<dyn LlmProvider> = common::fake_llm("unused");
        let mut module = common::load_module(&engine, "echo", llm);
        b.iter(|| {
            rt.block_on(async {
                let resp = module
                    .invoke_tool(ToolCallRequest {
                        name: "echo".to_string(),
                        arguments_json: r#"{"input":"hi"}"#.to_string(),
                        call_id: "bench".to_string(),
                        caller: None,
                    })
                    .await
                    .expect("invoke_tool succeeds");
                black_box(resp);
            });
        });
    });

    // Direct, from a runtime worker thread: how an agent's tool call or the
    // gateway's handler actually awaits it. The calls run inside one spawned
    // task (timed from inside it), so the spawn hop is not counted.
    group.bench_function("direct_wasm_module_invoke_tool_on_worker", |b| {
        let engine = common::engine();
        let llm: Arc<dyn LlmProvider> = common::fake_llm("unused");
        let mut module = Some(common::load_module(&engine, "echo", llm));
        b.iter_custom(|iters| {
            let mut m = module.take().expect("module is returned by every batch");
            let (m, elapsed) = rt
                .block_on(rt.spawn(async move {
                    let start = std::time::Instant::now();
                    for _ in 0..iters {
                        let resp = m
                            .invoke_tool(ToolCallRequest {
                                name: "echo".to_string(),
                                arguments_json: r#"{"input":"hi"}"#.to_string(),
                                call_id: "bench".to_string(),
                                caller: None,
                            })
                            .await
                            .expect("invoke_tool succeeds");
                        black_box(resp);
                    }
                    (m, start.elapsed())
                }))
                .expect("the bench task completes");
            module = Some(m);
            elapsed
        });
    });

    // Through the gateway, MCP `tools/call`.
    group.bench_function("mcp_tools_call", |b| {
        let (base, _llm) = rt.block_on(common::start_gateway(&["echo"]));
        let http = reqwest::Client::new();
        let url = format!("{base}/mcp/echo");
        let mut n = 0u64;
        b.iter(|| {
            n += 1;
            rt.block_on(async {
                let resp = http
                    .post(&url)
                    .json(&json!({
                        "jsonrpc": "2.0",
                        "id": n,
                        "method": "tools/call",
                        "params": {"name": "echo", "arguments": {"input": "hi"}},
                    }))
                    .send()
                    .await
                    .expect("request succeeds");
                black_box(resp.status());
            });
        });
    });

    group.finish();
}

criterion_group!(benches, tool_shaped);
criterion_main!(benches);
