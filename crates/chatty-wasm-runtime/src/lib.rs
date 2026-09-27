//! `chatty-wasm-runtime` — Wasmtime embedding for chatty WASM modules.
//!
//! Provides [`WasmModule`] which loads a WASM component compiled to
//! `wasm32-wasip2`, enforces per-call resource limits (fuel, wall-clock via
//! epoch interruption, memory, output size; see [`ResourceLimits`]), and
//! implements the host-side WIT interface (`llm`, `config`, `logging`,
//! `file`, `billing`).

mod error;
mod host;
mod limits;
mod module;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use error::CallError;
pub use host::{BillingProvider, LlmProvider, ModuleManifest};
pub use limits::{
    EPOCH_TICK, MAX_EXECUTION_MS_CEILING, MAX_FILE_READ_BYTES, MAX_FUEL_CEILING,
    MAX_MEMORY_BYTES_CEILING, MAX_OUTPUT_BYTES_CEILING, METADATA_CALL_MS, ResourceLimits,
};
pub use module::{InvocationMetrics, WasmModule};

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
