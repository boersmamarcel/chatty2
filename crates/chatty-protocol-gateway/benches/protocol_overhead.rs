//! S7 §2: per-call overhead of each protocol vs a direct in-process call
//! (AGE-603, plugin evaluation plan §3 S7).
//!
//! `chat`-shaped operations (direct `WasmModule::chat`, OpenAI, A2A) are one
//! group; `invoke_tool`-shaped operations (direct `WasmModule::invoke_tool`,
//! MCP `tools/call`) are the other — OpenAI and A2A have no tool-invocation
//! route of their own, and MCP has no free-form chat route, so those are the
//! honest pairings.
//!
//! Every module is `echo-agent` with a scripted `FakeLlm` returning
//! instantly; `echo-agent`'s `chat` only calls the LLM when the message
//! contains "use llm", so both groups exercise the same guest code path.
//!
//! Run: `CARGO_TARGET_DIR=<dir> cargo bench -p chatty-protocol-gateway --bench protocol_overhead`

#[path = "common.rs"]
mod common;

use chatty_wasm_runtime::{ChatRequest, LlmProvider, Message, Role};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use std::sync::Arc;

fn chat_shaped(c: &mut Criterion) {
    let rt = common::rt();
    let mut group = c.benchmark_group("chat_shaped");
    group.sample_size(30);

    // Direct: WasmModule::chat, no HTTP, no gateway.
    group.bench_function("direct_wasm_module_chat", |b| {
        let engine = common::engine();
        let llm: Arc<dyn LlmProvider> = common::fake_llm("from the model");
        let mut module = common::load_module(&engine, "echo-agent", llm);
        b.iter(|| {
            rt.block_on(async {
                let resp = module
                    .chat(ChatRequest {
                        messages: vec![Message {
                            role: Role::User,
                            content: "use llm: hello".to_string(),
                        }],
                        conversation_id: "bench".to_string(),
                    })
                    .await
                    .expect("chat succeeds");
                black_box(resp);
            });
        });
    });

    // Through the gateway, OpenAI-compatible route.
    group.bench_function("openai_route", |b| {
        let (base, _llm) = rt.block_on(common::start_gateway(&["echo-agent"]));
        let http = reqwest::Client::new();
        let url = format!("{base}/v1/echo-agent/chat/completions");
        b.iter(|| {
            rt.block_on(async {
                let resp = http
                    .post(&url)
                    .json(&json!({
                        "model": "echo-agent",
                        "messages": [{"role": "user", "content": "use llm: hello"}],
                    }))
                    .send()
                    .await
                    .expect("request succeeds");
                black_box(resp.status());
            });
        });
    });

    // Through the gateway, A2A `message/send`.
    group.bench_function("a2a_route", |b| {
        let (base, _llm) = rt.block_on(common::start_gateway(&["echo-agent"]));
        let http = reqwest::Client::new();
        let url = format!("{base}/a2a/echo-agent");
        let mut n = 0u64;
        b.iter(|| {
            n += 1;
            rt.block_on(async {
                let resp = http
                    .post(&url)
                    .json(&json!({
                        "jsonrpc": "2.0",
                        "id": n,
                        "method": "message/send",
                        "params": {
                            "message": {
                                "role": "user",
                                "messageId": format!("m{n}"),
                                "contextId": format!("ctx{n}"),
                                "parts": [{"kind": "text", "text": "use llm: hello"}],
                            }
                        }
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

fn tool_shaped(c: &mut Criterion) {
    let rt = common::rt();
    let mut group = c.benchmark_group("tool_shaped");
    group.sample_size(30);

    // Direct: WasmModule::invoke_tool, no HTTP, no gateway.
    group.bench_function("direct_wasm_module_invoke_tool", |b| {
        let engine = common::engine();
        let llm: Arc<dyn LlmProvider> = common::fake_llm("unused");
        let mut module = common::load_module(&engine, "echo-agent", llm);
        b.iter(|| {
            rt.block_on(async {
                let resp = module
                    .invoke_tool("echo", r#"{"input":"hi"}"#)
                    .await
                    .expect("invoke_tool succeeds");
                black_box(resp);
            });
        });
    });

    // Through the gateway, MCP `tools/call`.
    group.bench_function("mcp_tools_call", |b| {
        let (base, _llm) = rt.block_on(common::start_gateway(&["echo-agent"]));
        let http = reqwest::Client::new();
        let url = format!("{base}/mcp/echo-agent");
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

criterion_group!(benches, chat_shaped, tool_shaped);
criterion_main!(benches);
