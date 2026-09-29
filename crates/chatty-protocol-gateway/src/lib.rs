//! `chatty-protocol-gateway` — HTTP server exposing WASM plugins' tools over
//! MCP, and the broker's agents (local participants and virtual agents) over
//! A2A.
//!
//! # Routes
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `GET`  | `/` | JSON list of modules and endpoints |
//! | `GET`  | `/.well-known/agent.json` | Aggregated A2A agent card |
//! | `POST` | `/mcp/{module}` | MCP JSON-RPC (`tools/list`, `tools/call`) |
//! | `GET`  | `/mcp/{module}/sse` | MCP SSE transport: opens the event stream |
//! | `POST` | `/mcp/{module}/sse?sessionId=…` | MCP SSE transport: a client message |
//! | `GET`  | `/a2a/{agent}/.well-known/agent.json` | Per-agent A2A card |
//! | `POST` | `/a2a/{agent}` | A2A JSON-RPC (`message/send`, `message/stream`, `tasks/get`) |
//!
//! A `chatty:plugin@0.3.0` plugin contributes tools and never runs a loop of
//! its own (PL-D1 option B), so it is served over MCP only, and only when its
//! `[protocols] mcp` flag is set; otherwise it answers 404. There is no
//! OpenAI route and no module A2A route (PL-U3). Every route refuses a
//! request without the gateway's per-launch token, a non-loopback `Host` or
//! `Origin` (DNS rebinding) and a body over [`MAX_REQUEST_BYTES`].
//!
//! The gateway listens on a Unix socket in an owner-only directory, never on
//! a TCP port, with its token in a `0600` file beside it (ADR-0021 § 4; see
//! [`access`]).
//!
//! `{agent}` resolves a *local participant* — a process registered over the
//! participant socket (see [`participant`]) — or a virtual agent.
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
//! let mut gateway = ProtocolGateway::new(shared);
//! gateway.start().await?;
//! # Ok(())
//! # }
//! ```

pub mod access;
mod gateway;
mod handlers;
mod loopback;
pub mod participant;

/// The other end of the participant socket: what a process runs when it *is*
/// a worker. Behind the `worker` feature, which is the only thing here that
/// needs `chatty-core`.
#[cfg(feature = "worker")]
pub mod worker;

pub use access::GatewayToken;
pub use gateway::{GatewayState, MAX_REQUEST_BYTES, ProtocolGateway, RouteCounter};
