//! Gateway conformance suite: S3 of the WASM plugin evaluation plan
//! (AGE-599), rows 3.1–3.14.
//!
//! Every test starts a real gateway on an ephemeral port with real modules
//! loaded (`echo-agent`, `benford-agent`, and PL-E1 fixtures such as
//! `tool-args`, `slow-host`, `log-flood`, `huge-output`) and one scripted
//! [`FakeLlm`](chatty_wasm_runtime::test_support::FakeLlm) behind every
//! module's `llm::complete`. Where a row can be driven by a real client it
//! is: the official OpenAI Python package (3.4), rmcp and the MCP Inspector
//! CLI (3.6), and chatty-core's `A2aClient` (3.8, 3.9, 3.14).
//!
//! # Prerequisites
//!
//! The modules are build output: run `scripts/build-wasm-fixtures.sh` once
//! per checkout. The Python client needs `uv`, the Inspector needs `npx`; a
//! test whose tool is missing prints `SKIP <row>: <reason>` to stderr and
//! passes. Those two fetch their package (PyPI, npm) on first use — the only
//! network any test here touches. Nothing reaches a model.
//!
//! # Red rows
//!
//! Each test asserts the plan's pass criterion, not today's behaviour. A row
//! that fails today carries `#[ignore = "known defect: <issue>"]` and nothing
//! else; the fix issue removes the attribute. `cargo test -p
//! chatty-protocol-gateway --test conformance -- --ignored` lists them.
//! Rows the plan leaves open (3.3, 3.9, 3.13) record the decision taken in
//! the test's doc comment.

mod a2a;
mod gateway;
mod harness;
mod mcp;
mod openai;
