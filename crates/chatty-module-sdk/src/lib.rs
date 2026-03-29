//! `chatty-module-sdk` — SDK for authoring chatty WASM agent modules.
//!
//! This crate targets `wasm32-wasip2` and provides:
//!
//! - **Types** re-exported from the WIT interface (`ChatRequest`, `ChatResponse`, etc.)
//! - **Host imports** (`llm::complete`, `config::get`, `log::info`, etc.)
//! - **[`ModuleExports`] trait** — the trait module authors implement
//! - **[`export_module!`] macro** — wires the trait impl to the WIT guest exports
//!
//! # Quick start
//!
//! ```rust,ignore
//! use chatty_module_sdk::*;
//!
//! #[derive(Default)]
//! struct MyAgent;
//!
//! impl ModuleExports for MyAgent {
//!     fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
//!         // Call the host LLM
//!         let resp = llm::complete("claude-sonnet-4-20250514", &req.messages, None)?;
//!         Ok(ChatResponse {
//!             content: resp.content,
//!             tool_calls: vec![],
//!             usage: resp.usage,
//!         })
//!     }
//!
//!     fn invoke_tool(&self, _name: String, _args: String) -> Result<String, String> {
//!         Err("no tools".into())
//!     }
//!
//!     fn list_tools(&self) -> Vec<ToolDefinition> {
//!         vec![]
//!     }
//!
//!     fn get_agent_card(&self) -> AgentCard {
//!         AgentCard {
//!             name: "my-agent".into(),
//!             display_name: "My Agent".into(),
//!             description: "A demo agent".into(),
//!             version: "0.1.0".into(),
//!             skills: vec![],
//!             tools: vec![],
//!         }
//!     }
//! }
//!
//! export_module!(MyAgent);
//! ```

// ---------------------------------------------------------------------------
// WIT guest-side bindings
// ---------------------------------------------------------------------------
// Generated at the crate root so that types, import wrappers, and export
// helper functions (the `_export_*_cabi` family) are all accessible via
// `$crate::` from the `export_module!` macro.
wit_bindgen::generate!({
    world: "module",
    path: "../../wit",
});

// ---------------------------------------------------------------------------
// Re-export WIT types for module authors
// ---------------------------------------------------------------------------

pub use chatty::module::types::{
    AgentCard, ChatRequest, ChatResponse, CompletionResponse, Message, Role, Skill, TokenUsage,
    ToolCall, ToolDefinition,
};

// ---------------------------------------------------------------------------
// Host import wrappers
// ---------------------------------------------------------------------------

/// Host-provided LLM completion service.
///
/// Wraps the `llm::complete` host import with a typed Rust API.
pub mod llm {
    pub use super::{CompletionResponse, Message};

    /// Run a completion against a host-managed LLM model.
    ///
    /// # Arguments
    /// * `model`    — model identifier (e.g. `"claude-sonnet-4-20250514"`)
    /// * `messages` — conversation history
    /// * `tools`    — optional JSON-encoded tool definitions for the LLM
    ///
    /// # Returns
    /// The completion response or an error string from the host.
    pub fn complete(
        model: &str,
        messages: &[Message],
        tools: Option<&str>,
    ) -> Result<CompletionResponse, String> {
        super::chatty::module::llm::complete(model, messages, tools)
    }
}

/// Host-provided key-value configuration.
///
/// Wraps the `config::get` host import.  Configuration values are set in the
/// module's manifest on the host side.
pub mod config {
    /// Retrieve a configuration value by key.
    ///
    /// Returns `None` if the key is not set in the module's manifest.
    pub fn get(key: &str) -> Option<String> {
        super::chatty::module::config::get(key)
    }
}

/// Host-provided structured logging.
///
/// Convenience wrappers around the `logging::log` host import, one per log level.
pub mod log {
    /// Log at **trace** level.
    pub fn trace(message: &str) {
        super::chatty::module::logging::log("trace", message);
    }

    /// Log at **debug** level.
    pub fn debug(message: &str) {
        super::chatty::module::logging::log("debug", message);
    }

    /// Log at **info** level.
    pub fn info(message: &str) {
        super::chatty::module::logging::log("info", message);
    }

    /// Log at **warn** level.
    pub fn warn(message: &str) {
        super::chatty::module::logging::log("warn", message);
    }

    /// Log at **error** level.
    pub fn error(message: &str) {
        super::chatty::module::logging::log("error", message);
    }
}

// ---------------------------------------------------------------------------
// ModuleExports trait
// ---------------------------------------------------------------------------

/// The trait every chatty WASM module must implement.
///
/// Implement this on a `#[derive(Default)]` struct, then call
/// [`export_module!`] to wire it to the WIT guest exports.
///
/// The module is instantiated lazily on the first guest export call
/// via [`Default::default()`].
pub trait ModuleExports: Default + 'static {
    /// Handle a chat request and return a response.
    ///
    /// May call host imports ([`llm::complete`], [`config::get`],
    /// [`log::info`], etc.) during execution.
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String>;

    /// Invoke a tool exposed by this module.
    ///
    /// * `name` — tool name (must match a name from [`list_tools`](Self::list_tools))
    /// * `args` — JSON-encoded arguments
    ///
    /// Returns JSON-encoded output or an error string.
    fn invoke_tool(&self, name: String, args: String) -> Result<String, String>;

    /// List all tools this module provides.
    fn list_tools(&self) -> Vec<ToolDefinition>;

    /// Return the agent's metadata card.
    fn get_agent_card(&self) -> AgentCard;
}

// ---------------------------------------------------------------------------
// export_module! macro
// ---------------------------------------------------------------------------

/// Wire a [`ModuleExports`] implementation to the WIT guest exports.
///
/// Call this macro **exactly once** at the crate root of your module,
/// passing the type that implements [`ModuleExports`].
///
/// ```rust,ignore
/// export_module!(MyAgent);
/// ```
///
/// The macro creates a lazily-initialised instance of your type (via
/// [`Default::default()`]) and delegates every WIT export to the
/// corresponding [`ModuleExports`] method.
#[macro_export]
macro_rules! export_module {
    ($t:ty) => {
        // Private wrapper that implements the generated Guest trait.
        struct __ChattyGuest;

        impl $crate::exports::chatty::module::agent::Guest for __ChattyGuest {
            fn chat(
                req: $crate::ChatRequest,
            ) -> ::core::result::Result<$crate::ChatResponse, ::std::string::String> {
                static INSTANCE: ::std::sync::OnceLock<$t> = ::std::sync::OnceLock::new();
                let inst =
                    INSTANCE.get_or_init(|| <$t as ::core::default::Default>::default());
                <$t as $crate::ModuleExports>::chat(inst, req)
            }

            fn invoke_tool(
                name: ::std::string::String,
                args: ::std::string::String,
            ) -> ::core::result::Result<::std::string::String, ::std::string::String> {
                static INSTANCE: ::std::sync::OnceLock<$t> = ::std::sync::OnceLock::new();
                let inst =
                    INSTANCE.get_or_init(|| <$t as ::core::default::Default>::default());
                <$t as $crate::ModuleExports>::invoke_tool(inst, name, args)
            }

            fn list_tools() -> ::std::vec::Vec<$crate::ToolDefinition> {
                static INSTANCE: ::std::sync::OnceLock<$t> = ::std::sync::OnceLock::new();
                let inst =
                    INSTANCE.get_or_init(|| <$t as ::core::default::Default>::default());
                <$t as $crate::ModuleExports>::list_tools(inst)
            }

            fn get_agent_card() -> $crate::AgentCard {
                static INSTANCE: ::std::sync::OnceLock<$t> = ::std::sync::OnceLock::new();
                let inst =
                    INSTANCE.get_or_init(|| <$t as ::core::default::Default>::default());
                <$t as $crate::ModuleExports>::get_agent_card(inst)
            }
        }

        // Generate the component-model ABI glue that wires the WASM export
        // names to the SDK's type-erased cabi helpers.
        const _: () = {
            #[unsafe(export_name = "chatty:module/agent@0.1.0#chat")]
            unsafe extern "C" fn __chatty_export_chat(
                arg0: *mut u8,
                arg1: usize,
                arg2: *mut u8,
                arg3: usize,
            ) -> *mut u8 {
                unsafe {
                    $crate::exports::chatty::module::agent::_export_chat_cabi::<
                        __ChattyGuest,
                    >(arg0, arg1, arg2, arg3)
                }
            }

            #[unsafe(export_name = "cabi_post_chatty:module/agent@0.1.0#chat")]
            unsafe extern "C" fn __chatty_post_return_chat(arg0: *mut u8) {
                unsafe {
                    $crate::exports::chatty::module::agent::__post_return_chat::<
                        __ChattyGuest,
                    >(arg0)
                }
            }

            #[unsafe(export_name = "chatty:module/agent@0.1.0#invoke-tool")]
            unsafe extern "C" fn __chatty_export_invoke_tool(
                arg0: *mut u8,
                arg1: usize,
                arg2: *mut u8,
                arg3: usize,
            ) -> *mut u8 {
                unsafe {
                    $crate::exports::chatty::module::agent::_export_invoke_tool_cabi::<
                        __ChattyGuest,
                    >(arg0, arg1, arg2, arg3)
                }
            }

            #[unsafe(export_name = "cabi_post_chatty:module/agent@0.1.0#invoke-tool")]
            unsafe extern "C" fn __chatty_post_return_invoke_tool(arg0: *mut u8) {
                unsafe {
                    $crate::exports::chatty::module::agent::__post_return_invoke_tool::<
                        __ChattyGuest,
                    >(arg0)
                }
            }

            #[unsafe(export_name = "chatty:module/agent@0.1.0#list-tools")]
            unsafe extern "C" fn __chatty_export_list_tools() -> *mut u8 {
                unsafe {
                    $crate::exports::chatty::module::agent::_export_list_tools_cabi::<
                        __ChattyGuest,
                    >()
                }
            }

            #[unsafe(export_name = "cabi_post_chatty:module/agent@0.1.0#list-tools")]
            unsafe extern "C" fn __chatty_post_return_list_tools(arg0: *mut u8) {
                unsafe {
                    $crate::exports::chatty::module::agent::__post_return_list_tools::<
                        __ChattyGuest,
                    >(arg0)
                }
            }

            #[unsafe(export_name = "chatty:module/agent@0.1.0#get-agent-card")]
            unsafe extern "C" fn __chatty_export_get_agent_card() -> *mut u8 {
                unsafe {
                    $crate::exports::chatty::module::agent::_export_get_agent_card_cabi::<
                        __ChattyGuest,
                    >()
                }
            }

            #[unsafe(export_name = "cabi_post_chatty:module/agent@0.1.0#get-agent-card")]
            unsafe extern "C" fn __chatty_post_return_get_agent_card(arg0: *mut u8) {
                unsafe {
                    $crate::exports::chatty::module::agent::__post_return_get_agent_card::<
                        __ChattyGuest,
                    >(arg0)
                }
            }
        };
    };
}
