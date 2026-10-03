//! `chatty-wasm-runtime` — Wasmtime embedding for chatty WASM plugins.
//!
//! Provides [`WasmModule`], which loads a component compiled to
//! `wasm32-wasip2` against `chatty:plugin@0.3.0` (and refuses any other
//! world with a message asking for a rebuild), enforces per-call resource
//! limits (fuel, wall-clock via epoch interruption, memory, output size; see
//! [`ResourceLimits`]), and implements the host side of the plugin imports
//! (`llm`, `config`, `logging`, `file`, `billing`), linking only the
//! capabilities the module was granted ([`Grants`], PL-U4).

mod error;
mod grants;
mod host;
mod limits;
mod module;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use error::{CallError, ToolFailure};
pub use grants::{Grants, NotGranted, SPECLESS_DEFAULTS, UnrequestedGrant};
pub use host::{BillingProvider, LlmProvider, ModuleManifest};
pub use limits::{
    EPOCH_TICK, MAX_EXECUTION_MS_CEILING, MAX_FILE_READ_BYTES, MAX_FUEL_CEILING,
    MAX_MEMORY_BYTES_CEILING, MAX_OUTPUT_BYTES_CEILING, METADATA_CALL_MS, ResourceLimits,
};
pub use module::{InvocationMetrics, WasmModule};

/// Host-side WIT types re-exported for callers.
pub use bindings::chatty::plugin::billing::SessionInfo;
pub use bindings::chatty::plugin::types::{
    CompletionResponse, Message, Role, TokenUsage, ToolCall, ToolDefinition,
};
pub use bindings::exports::chatty::plugin::plugin::{
    Capability, ConfigKey, PluginMetadata, ToolCallRequest, ToolError, ToolErrorKind, ToolResult,
};

/// Re-export the wasmtime [`Engine`] so callers can share one engine across
/// multiple modules without a direct wasmtime dependency.
pub use wasmtime::Engine;

/// The one WIT package this host loads. Every other world is refused at load
/// with a message asking for a rebuild (PL-D1: no older world is adapted).
pub const WIT_PACKAGE: &str = "chatty:plugin@0.3.0";

/// The export a plugin must provide: `WIT_PACKAGE`'s `plugin` interface.
pub const PLUGIN_EXPORT: &str = "chatty:plugin/plugin@0.3.0";

// A WIT version bump has to update the two constants above (and the SDK's);
// the build fails loudly until they agree with the file.
const _: () = assert!(
    declares_package(
        include_str!("../../../wit/chatty-plugin.wit"),
        "package chatty:plugin@0.3.0;"
    ),
    "wit/chatty-plugin.wit no longer declares `package chatty:plugin@0.3.0;`: \
     a WIT version bump must also update WIT_PACKAGE/PLUGIN_EXPORT in \
     chatty-wasm-runtime and WIT_PACKAGE in chatty-module-sdk"
);

/// Whether `wit` contains `line` (a const-evaluable substring search).
const fn declares_package(wit: &str, line: &str) -> bool {
    let (wit, line) = (wit.as_bytes(), line.as_bytes());
    let mut start = 0;
    while start + line.len() <= wit.len() {
        let mut i = 0;
        while i < line.len() && wit[start + i] == line[i] {
            i += 1;
        }
        if i == line.len() {
            return true;
        }
        start += 1;
    }
    false
}

/// Generated host-side bindings from the WIT interface.
///
/// The macro reads `../../wit/` relative to this crate's `Cargo.toml`.
pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "plugin-world",
        path: "../../wit",
        // `config::get` has no error channel, so an ungranted one traps
        // (`grants::Refused`).
        trappable_imports: ["get"],
    });
}
