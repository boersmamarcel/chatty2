use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, trace, warn};
use wasmtime_wasi::{IoView, ResourceTable, WasiCtx, WasiCtxBuilder, WasiView};

use crate::bindings::chatty::module::billing::SessionInfo;
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
// BillingProvider trait
// ---------------------------------------------------------------------------

/// Callback interface for billing session management (Phase 3b).
///
/// The host implements this to forward billing requests to the Hive registry.
/// Called synchronously from WASM; use `block_on` if the implementation is async.
///
/// # Example Implementation
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use chatty_wasm_runtime::BillingProvider;
/// use chatty_wasm_runtime::bindings::chatty::module::billing::SessionInfo;
///
/// struct HiveBillingProvider {
///     hive_client: Arc<hive_client::HiveRegistryClient>,
///     module_name: String,
///     module_version: String,
///     session_id: std::sync::Mutex<Option<String>>,
/// }
///
/// impl BillingProvider for HiveBillingProvider {
///     fn acquire_session(&self, estimated_tokens: i64) -> Result<SessionInfo, String> {
///         // Call Hive's acquire-session API (async, so we block_on)
///         let response = tokio::runtime::Handle::current()
///             .block_on(async {
///                 self.hive_client
///                     .acquire_session(&self.module_name, &self.module_version, estimated_tokens)
///                     .await
///             })
///             .map_err(|e| format!("acquire-session failed: {}", e))?;
///
///         // Store session ID for later settlement
///         *self.session_id.lock().unwrap() = Some(response.session_id.clone());
///
///         Ok(SessionInfo {
///             token: response.token,
///             balance_tokens: response.balance_tokens,
///             reserved_tokens: response.reserved_tokens,
///             pricing_model: response.pricing_model,
///         })
///     }
///
///     fn report_usage(&self, input_tokens: i64, output_tokens: i64) -> Result<(), String> {
///         let session_id = self.session_id
///             .lock()
///             .unwrap()
///             .as_ref()
///             .ok_or("no active session")?
///             .clone();
///
///         tokio::runtime::Handle::current()
///             .block_on(async {
///                 self.hive_client
///                     .settle_session(&session_id, input_tokens, output_tokens)
///                     .await
///             })
///             .map_err(|e| format!("settle-session failed: {}", e))?;
///
///         Ok(())
///     }
/// }
/// ```
pub trait BillingProvider: Send + Sync {
    /// Acquire a billing session before module execution.
    ///
    /// Reserves credits on behalf of the user and returns a signed JWT
    /// that the module can verify.
    fn acquire_session(&self, estimated_tokens: i64) -> Result<SessionInfo, String>;

    /// Report actual usage after module execution.
    ///
    /// Settles the session by deducting actual usage and releasing
    /// the reserved remainder.
    fn report_usage(&self, input_tokens: i64, output_tokens: i64) -> Result<(), String>;
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
    /// Callback for billing session management.
    pub(crate) billing_provider: Option<Arc<dyn BillingProvider>>,
    /// WASI Preview 2 context — provides the WASI host implementations
    /// required by modules compiled for `wasm32-wasip2`.
    pub(crate) wasi_ctx: WasiCtx,
    /// WASI resource table for tracking guest resources.
    pub(crate) table: ResourceTable,
    /// Optional channel for streaming progress events to the gateway.
    pub(crate) progress_tx: Option<UnboundedSender<String>>,
}

impl ModuleState {
    pub(crate) fn new(
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        billing_provider: Option<Arc<dyn BillingProvider>>,
        resource_limits: &ResourceLimits,
    ) -> Self {
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(resource_limits.max_memory_bytes as usize)
            .build();

        // Minimal WASI context — no filesystem, no network, no env vars.
        // Modules compiled for wasm32-wasip2 import these interfaces from
        // the host; we satisfy them with a sandboxed no-op implementation.
        let wasi_ctx = WasiCtxBuilder::new().build();
        let table = ResourceTable::new();

        Self {
            limits,
            manifest,
            llm_provider,
            billing_provider,
            wasi_ctx,
            table,
            progress_tx: None,
        }
    }
}

// Implement IoView (required by WasiView) so WASI can access the resource table.
impl IoView for ModuleState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

// Implement WasiView so the WASI linker can access the context and table.
impl WasiView for ModuleState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi_ctx
    }
}

// `WasiCtx` is `!Sync` due to internal `UnsafeCell` usage, but `ModuleState`
// is ONLY ever accessed through `&mut Store<ModuleState>` (exclusive access)
// inside a `WasmModule` that is guarded by the `RwLock<ModuleRegistry>` write
// lock.  No shared `&ModuleState` reference can reach another thread; the
// `Sync` bound is required purely so `Arc<RwLock<ModuleRegistry>>` satisfies
// axum's `Send + Sync` state constraint.
//
// Safety: see above — no concurrent shared-reference access ever occurs.
unsafe impl Sync for ModuleState {}

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
        // Forward to progress channel for real-time streaming
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.send(message);
        }
    }
}

impl crate::bindings::chatty::module::billing::Host for ModuleState {
    fn acquire_session(&mut self, estimated_tokens: i64) -> Result<SessionInfo, String> {
        debug!(
            module = %self.manifest.name,
            estimated_tokens = estimated_tokens,
            "billing::acquire-session called by WASM module"
        );

        match &self.billing_provider {
            Some(provider) => {
                let result = provider.acquire_session(estimated_tokens);
                if let Err(ref e) = result {
                    warn!(module = %self.manifest.name, error = %e, "billing::acquire-session failed");
                }
                result
            }
            None => {
                // No billing provider — module is calling billing but host doesn't support it.
                // This is an error: paid modules require a billing provider.
                error!(module = %self.manifest.name, "billing::acquire-session called but no BillingProvider configured");
                Err("billing not configured on host".to_string())
            }
        }
    }

    fn report_usage(&mut self, input_tokens: i64, output_tokens: i64) -> Result<(), String> {
        debug!(
            module = %self.manifest.name,
            input_tokens = input_tokens,
            output_tokens = output_tokens,
            "billing::report-usage called by WASM module"
        );

        match &self.billing_provider {
            Some(provider) => {
                let result = provider.report_usage(input_tokens, output_tokens);
                if let Err(ref e) = result {
                    warn!(module = %self.manifest.name, error = %e, "billing::report-usage failed");
                }
                result
            }
            None => {
                error!(module = %self.manifest.name, "billing::report-usage called but no BillingProvider configured");
                Err("billing not configured on host".to_string())
            }
        }
    }
}

impl crate::bindings::chatty::module::file::Host for ModuleState {
    fn read_bytes(&mut self, path: String) -> Result<Vec<u8>, String> {
        // Resolve weights-root from the module's own config.
        let root = self
            .manifest
            .get_config("weights_root")
            .ok_or_else(|| "file::read_bytes: `weights_root` not configured".to_string())?;

        // Sandbox: reject absolute paths and any `..` component.
        if Path::new(&path).is_absolute() {
            return Err(format!("file::read_bytes: absolute path rejected: {path}"));
        }
        if path.split('/').any(|seg| seg == "..") {
            return Err(format!("file::read_bytes: `..` component rejected: {path}"));
        }

        let full = Path::new(&root).join(&path);

        debug!(
            module = %self.manifest.name,
            path = %full.display(),
            "file::read_bytes"
        );

        std::fs::read(&full).map_err(|e| {
            warn!(module = %self.manifest.name, path = %full.display(), error = %e, "file::read_bytes failed");
            format!("file::read_bytes: {e}")
        })
    }
}

// ---------------------------------------------------------------------------
// Backwards-compat host impls for chatty:module@0.1.0
// ---------------------------------------------------------------------------
//
// These delegate to the same business logic as the @0.2.0 impls. Bindgen
// generates separate (but structurally identical) Host traits per package
// version, so we need explicit impls. Types are also distinct nominal types,
// so we convert at the boundary.

impl crate::bindings_v0_1::chatty::module::types::Host for ModuleState {}

impl crate::bindings_v0_1::chatty::module::llm::Host for ModuleState {
    fn complete(
        &mut self,
        model: String,
        messages: Vec<crate::bindings_v0_1::chatty::module::types::Message>,
        tools: Option<String>,
    ) -> Result<crate::bindings_v0_1::chatty::module::types::CompletionResponse, String> {
        debug!(
            module = %self.manifest.name,
            model = %model,
            message_count = messages.len(),
            "llm::complete (v0.1) called by WASM module"
        );
        // Convert v0.1 messages -> v0.2 (identical layout)
        let v2_messages: Vec<Message> = messages
            .into_iter()
            .map(|m| Message {
                role: match m.role {
                    crate::bindings_v0_1::chatty::module::types::Role::System => {
                        crate::bindings::chatty::module::types::Role::System
                    }
                    crate::bindings_v0_1::chatty::module::types::Role::User => {
                        crate::bindings::chatty::module::types::Role::User
                    }
                    crate::bindings_v0_1::chatty::module::types::Role::Assistant => {
                        crate::bindings::chatty::module::types::Role::Assistant
                    }
                },
                content: m.content,
            })
            .collect();
        let result = self.llm_provider.complete(&model, v2_messages, tools);
        if let Err(ref e) = result {
            warn!(module = %self.manifest.name, error = %e, "llm::complete (v0.1) returned error");
        }
        // Convert v0.2 response back to v0.1
        result.map(
            |r| crate::bindings_v0_1::chatty::module::types::CompletionResponse {
                content: r.content,
                usage: r.usage.map(
                    |u| crate::bindings_v0_1::chatty::module::types::TokenUsage {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                    },
                ),
                tool_calls: r
                    .tool_calls
                    .into_iter()
                    .map(|tc| crate::bindings_v0_1::chatty::module::types::ToolCall {
                        id: tc.id,
                        name: tc.name,
                        arguments: tc.arguments,
                    })
                    .collect(),
            },
        )
    }
}

impl crate::bindings_v0_1::chatty::module::config::Host for ModuleState {
    fn get(&mut self, key: String) -> Option<String> {
        self.manifest.get_config(&key)
    }
}

impl crate::bindings_v0_1::chatty::module::logging::Host for ModuleState {
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
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.send(message);
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
        ModuleState::new(manifest, provider, None, &ResourceLimits::default())
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
    fn module_state_initializes() {
        let provider: Arc<dyn LlmProvider> = Arc::new(EchoProvider {
            response: String::new(),
        });
        let state = make_state(provider);
        assert_eq!(state.manifest.name, "test-module");
    }
}
