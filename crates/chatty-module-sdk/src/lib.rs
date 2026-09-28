//! `chatty-module-sdk` — the SDK for chatty WASM plugins (`chatty:plugin@0.3.0`).
//!
//! A plugin contributes tools to a chatty agent; the agent's own loop decides
//! when to call them (PL-D1 option B). Compile to `wasm32-wasip2`.
//!
//! - **Types** generated from `wit/chatty-plugin.wit` ([`ToolDefinition`],
//!   [`ToolCallRequest`], [`ToolResult`], [`ToolError`], [`PluginMetadata`], …)
//! - **Host imports**, one module per capability: [`llm`], [`config`],
//!   [`log`], [`file`], [`billing`]
//! - **[`Plugin`]**, the trait a plugin implements, and **[`export!`]**,
//!   which wires it to the component's exports. Both come from wit-bindgen's
//!   own generator, so the export names always match the WIT.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use chatty_module_sdk::*;
//!
//! struct Shout;
//!
//! impl Plugin for Shout {
//!     fn metadata() -> PluginMetadata {
//!         PluginMetadata {
//!             name: "shout".into(),
//!             version: "0.1.0".into(),
//!             description: "Upper-cases text".into(),
//!             requested_capabilities: vec![],
//!             config_keys: vec![],
//!         }
//!     }
//!
//!     fn list_tools() -> Vec<ToolDefinition> {
//!         vec![ToolDefinition {
//!             name: "shout".into(),
//!             description: "Upper-case the arguments".into(),
//!             parameters_schema: r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into(),
//!         }]
//!     }
//!
//!     fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
//!         match call.name.as_str() {
//!             "shout" => Ok(ToolResult::text(call.arguments_json.to_uppercase())),
//!             other => Err(ToolError::unknown_tool(other)),
//!         }
//!     }
//! }
//!
//! export!(Shout);
//! ```

// ---------------------------------------------------------------------------
// WIT guest-side bindings
// ---------------------------------------------------------------------------

wit_bindgen::generate!({
    world: "plugin-world",
    path: "../../wit",
    // The export macro is generated too (no hand-written symbol names), and
    // public so `export!` below can call it from a plugin crate.
    pub_export_macro: true,
    export_macro_name: "__export_plugin_world",
    default_bindings_module: "::chatty_module_sdk",
});

/// The WIT package this SDK builds plugins for. The host refuses any other.
pub const WIT_PACKAGE: &str = "chatty:plugin@0.3.0";

// A WIT version bump must be deliberate: it changes what every host accepts,
// so it has to update `WIT_PACKAGE` here and in chatty-wasm-runtime too. The
// build fails loudly until both agree with the file.
const _: () = assert!(
    declares_package(
        include_str!("../../../wit/chatty-plugin.wit"),
        "package chatty:plugin@0.3.0;"
    ),
    "wit/chatty-plugin.wit no longer declares `package chatty:plugin@0.3.0;`: \
     a WIT version bump must also update WIT_PACKAGE in chatty-module-sdk and \
     chatty-wasm-runtime"
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

// ---------------------------------------------------------------------------
// Re-exported WIT types
// ---------------------------------------------------------------------------

pub use chatty::plugin::types::{
    CompletionResponse, Message, Role, TokenUsage, ToolCall, ToolDefinition,
};
pub use exports::chatty::plugin::plugin::{
    Capability, ConfigKey, Guest as Plugin, PluginMetadata, ToolCallRequest, ToolError,
    ToolErrorKind, ToolResult,
};

/// Wire a [`Plugin`] implementation to the component's exports.
///
/// Call it exactly once, at the crate root of the plugin:
///
/// ```rust,ignore
/// chatty_module_sdk::export!(MyPlugin);
/// ```
#[macro_export]
macro_rules! export {
    ($plugin:ident) => {
        $crate::__export_plugin_world!($plugin with_types_in $crate);
    };
}

impl ToolResult {
    /// A result carrying `content` and no model usage.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            usage: None,
        }
    }
}

impl ToolError {
    /// An error of `kind` whose `message` the model sees.
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// No tool called `name`.
    pub fn unknown_tool(name: &str) -> Self {
        Self::new(ToolErrorKind::UnknownTool, format!("unknown tool: {name}"))
    }

    /// The arguments did not parse or did not match the schema.
    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::InvalidArguments, message)
    }

    /// A host capability refused the call.
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Denied, message)
    }

    /// The tool ran and failed.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Failed, message)
    }
}

impl Message {
    /// A message with no tool calls and no tool-call id.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Host imports, one module per capability
// ---------------------------------------------------------------------------

/// Capability `llm`: a completion on the calling agent's provider.
pub mod llm {
    pub use super::{CompletionResponse, Message};

    /// Run a completion. An empty `model` is the calling agent's model; a
    /// named one must be configured on the host. `tools` is an optional
    /// JSON-encoded list of tool definitions for the model.
    pub fn complete(
        model: &str,
        messages: &[Message],
        tools: Option<&str>,
    ) -> Result<CompletionResponse, String> {
        super::chatty::plugin::llm::complete(model, messages, tools)
    }
}

/// Capability `file`: read-only access below the plugin's granted file root.
pub mod file {
    /// Read the raw bytes of `path`, relative to the file root (no leading
    /// `/`, no `..`). A plugin granted no root reads nothing.
    pub fn read_bytes(path: &str) -> Result<Vec<u8>, String> {
        super::chatty::plugin::file::read_bytes(path)
    }
}

/// Capability `config`: values from the plugin's `[config]` table.
pub mod config {
    /// The value of `key`, or `None` when it is not set.
    pub fn get(key: &str) -> Option<String> {
        super::chatty::plugin::config::get(key)
    }
}

/// Capability `logging` (always granted), one function per level.
pub mod log {
    /// Log at **trace** level.
    pub fn trace(message: &str) {
        super::chatty::plugin::logging::log("trace", message);
    }

    /// Log at **debug** level.
    pub fn debug(message: &str) {
        super::chatty::plugin::logging::log("debug", message);
    }

    /// Log at **info** level.
    pub fn info(message: &str) {
        super::chatty::plugin::logging::log("info", message);
    }

    /// Log at **warn** level.
    pub fn warn(message: &str) {
        super::chatty::plugin::logging::log("warn", message);
    }

    /// Log at **error** level.
    pub fn error(message: &str) {
        super::chatty::plugin::logging::log("error", message);
    }
}

/// Capability `billing`: the raw Hive billing imports. Paid plugins use them
/// through `hive-billing-sdk`, which verifies the session token.
pub mod billing {
    pub use super::chatty::plugin::billing::SessionInfo;

    /// Reserve `estimated_tokens` credits before doing work.
    pub fn acquire_session(estimated_tokens: i64) -> Result<SessionInfo, String> {
        super::chatty::plugin::billing::acquire_session(estimated_tokens)
    }

    /// Report the actual usage; settles the session.
    pub fn report_usage(input_tokens: i64, output_tokens: i64) -> Result<(), String> {
        super::chatty::plugin::billing::report_usage(input_tokens, output_tokens)
    }
}
