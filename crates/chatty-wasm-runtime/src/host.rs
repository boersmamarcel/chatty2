use std::sync::Arc;

use tracing::{debug, error, info, trace, warn};

use crate::bindings::chatty::module::types::{CompletionResponse, Message};
use crate::limits::ResourceLimits;

// ---------------------------------------------------------------------------
// LlmProvider trait
// ---------------------------------------------------------------------------

/// Callback interface that the host supplies so WASM modules can call the
/// chatty LLM back-end.
///
/// The implementation is called synchronously from within the Wasmtime host
/// function for `llm::complete`.  If the underlying LLM client is async,
/// wrap the call with [`tokio::runtime::Handle::current().block_on`] or
/// keep the provider pre-built and cache the result.
pub trait LlmProvider: Send + Sync {
    /// Run a completion against the host-managed model.
    ///
    /// Returns the completion response on success, or an error string that
    /// the guest module will receive as the `result` error variant.
    fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String>;
}

// ---------------------------------------------------------------------------
// ModuleManifest
// ---------------------------------------------------------------------------

/// Static configuration supplied by a WASM module alongside its binary.
///
/// The manifest is read during loading; its key-value pairs are returned to
/// the guest when it calls the `config::get` host import.
#[derive(Debug, Clone, Default)]
pub struct ModuleManifest {
    /// Human-readable module name used as a prefix in log messages.
    pub name: String,
    /// Arbitrary key-value configuration the module declared.
    config: std::collections::HashMap<String, String>,
}

impl ModuleManifest {
    /// Create a new manifest with the given name and no config entries.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            config: Default::default(),
        }
    }

    /// Add or overwrite a configuration key-value pair.
    pub fn with_config(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.insert(key.into(), value.into());
        self
    }

    /// Look up a config value by key, returning `None` if not present.
    pub(crate) fn get_config(&self, key: &str) -> Option<String> {
        self.config.get(key).cloned()
    }
}

// ---------------------------------------------------------------------------
// ModuleState — per-module store data
// ---------------------------------------------------------------------------

/// Data stored inside the Wasmtime [`Store`](wasmtime::Store) for each
/// module instance.
///
/// Holds both the resource limiter (for memory caps) and the runtime
/// dependencies needed by host imports.
pub(crate) struct ModuleState {
    /// Wasmtime resource limiter (memory cap).
    pub(crate) limits: wasmtime::StoreLimits,
    /// Static module configuration.
    pub(crate) manifest: ModuleManifest,
    /// Callback for LLM completions.
    pub(crate) llm_provider: Arc<dyn LlmProvider>,
}

impl ModuleState {
    pub(crate) fn new(
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        resource_limits: &ResourceLimits,
    ) -> Self {
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(resource_limits.max_memory_bytes as usize)
            .build();

        Self {
            limits,
            manifest,
            llm_provider,
        }
    }
}

// ---------------------------------------------------------------------------
// Host import implementations
// ---------------------------------------------------------------------------

// The `types` interface only exports shared type definitions — no functions.
// Wasmtime's bindgen! still requires an empty `Host` impl.
impl crate::bindings::chatty::module::types::Host for ModuleState {}

impl crate::bindings::chatty::module::llm::Host for ModuleState {
    fn complete(
        &mut self,
        model: String,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        debug!(
            module = %self.manifest.name,
            model = %model,
            message_count = messages.len(),
            has_tools = tools.is_some(),
            "llm::complete called by WASM module"
        );
        let result = self.llm_provider.complete(&model, messages, tools);
        if let Err(ref e) = result {
            warn!(module = %self.manifest.name, error = %e, "llm::complete returned error");
        }
        result
    }
}

impl crate::bindings::chatty::module::config::Host for ModuleState {
    fn get(&mut self, key: String) -> Option<String> {
        let value = self.manifest.get_config(&key);
        debug!(
            module = %self.manifest.name,
            key = %key,
            found = value.is_some(),
            "config::get called by WASM module"
        );
        value
    }
}

impl crate::bindings::chatty::module::logging::Host for ModuleState {
    fn log(&mut self, level: String, message: String) {
        let module = &self.manifest.name;
        match level.as_str() {
            "trace" => trace!(module = %module, "{}", message),
            "debug" => debug!(module = %module, "{}", message),
            "info" => info!(module = %module, "{}", message),
            "warn" => warn!(module = %module, "{}", message),
            "error" => error!(module = %module, "{}", message),
            other => info!(module = %module, level = %other, "{}", message),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // A simple mock LLM provider for testing.
    struct EchoProvider {
        response: String,
    }

    impl LlmProvider for EchoProvider {
        fn complete(
            &self,
            _model: &str,
            messages: Vec<Message>,
            _tools: Option<String>,
        ) -> Result<CompletionResponse, String> {
            let last = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            Ok(CompletionResponse {
                content: format!("echo: {}{}", last, self.response),
                tool_calls: vec![],
                usage: None,
            })
        }
    }

    struct ErrorProvider;

    impl LlmProvider for ErrorProvider {
        fn complete(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _tools: Option<String>,
        ) -> Result<CompletionResponse, String> {
            Err("provider error".to_string())
        }
    }

    fn make_state(provider: Arc<dyn LlmProvider>) -> ModuleState {
        let manifest = ModuleManifest::new("test-module")
            .with_config("api_key", "secret123")
            .with_config("endpoint", "https://example.com");
        ModuleState::new(manifest, provider, &ResourceLimits::default())
    }

    #[test]
    fn module_manifest_config_lookup() {
        let manifest = ModuleManifest::new("my-module")
            .with_config("key1", "val1")
            .with_config("key2", "val2");

        assert_eq!(manifest.get_config("key1"), Some("val1".to_string()));
        assert_eq!(manifest.get_config("key2"), Some("val2".to_string()));
        assert_eq!(manifest.get_config("missing"), None);
        assert_eq!(manifest.name, "my-module");
    }

    #[test]
    fn module_manifest_overwrite() {
        let manifest = ModuleManifest::new("m")
            .with_config("k", "v1")
            .with_config("k", "v2");
        assert_eq!(manifest.get_config("k"), Some("v2".to_string()));
    }

    #[test]
    fn config_host_returns_value() {
        use crate::bindings::chatty::module::config::Host;
        let provider: Arc<dyn LlmProvider> = Arc::new(EchoProvider {
            response: String::new(),
        });
        let mut state = make_state(provider);
        assert_eq!(
            state.get("api_key".to_string()),
            Some("secret123".to_string())
        );
        assert_eq!(
            state.get("endpoint".to_string()),
            Some("https://example.com".to_string())
        );
        assert_eq!(state.get("unknown".to_string()), None);
    }

    #[test]
    fn llm_host_routes_to_provider() {
        use crate::bindings::chatty::module::llm::Host;
        let provider: Arc<dyn LlmProvider> = Arc::new(EchoProvider {
            response: "!".to_string(),
        });
        let mut state = make_state(provider);
        let messages = vec![Message {
            role: crate::bindings::chatty::module::types::Role::User,
            content: "hello".to_string(),
        }];
        let result = state.complete("gpt-4".to_string(), messages, None);
        let resp = result.unwrap();
        assert_eq!(resp.content, "echo: hello!");
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn llm_host_propagates_provider_error() {
        use crate::bindings::chatty::module::llm::Host;
        let provider: Arc<dyn LlmProvider> = Arc::new(ErrorProvider);
        let mut state = make_state(provider);
        let result = state.complete("gpt-4".to_string(), vec![], None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "provider error");
    }

    #[test]
    fn logging_host_does_not_panic() {
        use crate::bindings::chatty::module::logging::Host;
        let provider: Arc<dyn LlmProvider> = Arc::new(EchoProvider {
            response: String::new(),
        });
        let mut state = make_state(provider);
        // None of these should panic.
        state.log("trace".to_string(), "trace msg".to_string());
        state.log("debug".to_string(), "debug msg".to_string());
        state.log("info".to_string(), "info msg".to_string());
        state.log("warn".to_string(), "warn msg".to_string());
        state.log("error".to_string(), "error msg".to_string());
        state.log("unknown".to_string(), "unknown level msg".to_string());
    }

    #[test]
    fn module_state_initialises() {
        let provider: Arc<dyn LlmProvider> = Arc::new(EchoProvider {
            response: String::new(),
        });
        let state = make_state(provider);
        assert_eq!(state.manifest.name, "test-module");
    }
}
