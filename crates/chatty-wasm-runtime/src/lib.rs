//! `chatty-wasm-runtime` — Wasmtime embedding for chatty WASM modules.
//!
//! Provides [`WasmModule`] which loads a WASM component compiled to
//! `wasm32-wasip2`, manages resource limits, and implements the host-side
//! WIT interface (`llm`, `config`, `logging`, `process`, `http`).

mod host;
mod limits;
mod module;

pub use host::{
    HttpClient, HttpResponse, LlmProvider, ModuleManifest, ProcessExecutor, ProcessResult,
};
pub use limits::ResourceLimits;
pub use module::WasmModule;

/// Host-side WIT types re-exported for callers.
pub use bindings::chatty::module::types::{
    AgentCard, ChatRequest, ChatResponse, CompletionResponse, Message, Role, Skill, TokenUsage,
    ToolCall, ToolDefinition,
};

/// Re-export the wasmtime [`Engine`] so callers can share one engine across
/// multiple modules without a direct wasmtime dependency.
pub use wasmtime::Engine;

/// Generated host-side bindings from the WIT interface.
///
/// The macro reads `../../wit/` relative to this crate's `Cargo.toml`.
pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "module",
        path: "../../wit",
    });
}
