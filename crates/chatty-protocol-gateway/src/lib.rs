//! `chatty-protocol-gateway` — HTTP server exposing WASM modules via OpenAI,
//! MCP, and A2A protocols simultaneously.
//!
//! # Routes
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `GET`  | `/` | JSON list of modules and endpoints |
//! | `GET`  | `/.well-known/agent.json` | Aggregated A2A agent card |
//! | `POST` | `/v1/{module}/chat/completions` | OpenAI-compatible chat completion |
//! | `POST` | `/v1/chat/completions` | Routes by model field (`module:{name}`) |
//! | `POST` | `/mcp/{module}` | MCP JSON-RPC (`tools/list`, `tools/call`) |
//! | `GET`  | `/mcp/{module}/sse` | MCP SSE transport |
//! | `GET`  | `/a2a/{module}/.well-known/agent.json` | Per-module A2A agent card |
//! | `POST` | `/a2a/{module}` | A2A JSON-RPC (`message/send`, `tasks/get`) |
//!
//! # Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use tokio::sync::RwLock;
//! use chatty_module_registry::ModuleRegistry;
//! use chatty_protocol_gateway::ProtocolGateway;
//! use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
//!
//! # struct NoopProvider;
//! # impl LlmProvider for NoopProvider {
//! #     fn complete(&self, _: &str, _: Vec<chatty_wasm_runtime::Message>, _: Option<String>)
//! #         -> Result<chatty_wasm_runtime::CompletionResponse, String> { Err("noop".into()) }
//! # }
//! # async fn run() -> anyhow::Result<()> {
//! let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
//! let registry = ModuleRegistry::new(provider, ResourceLimits::default())?;
//! let shared = Arc::new(RwLock::new(registry));
//!
//! let mut gateway = ProtocolGateway::new(shared, 8080);
//! gateway.start().await?;
//! # Ok(())
//! # }
//! ```

mod gateway;
mod handlers;

pub use gateway::ProtocolGateway;
