use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, trace, warn};
use wasmtime::{ResourceLimiter, StoreLimits};
use wasmtime_wasi::{
    IoView, OutputStream, Pollable, ResourceTable, StdoutStream, StreamError, WasiCtx,
    WasiCtxBuilder, WasiView,
};

use crate::bindings::chatty::module::billing::SessionInfo;
use crate::bindings::chatty::module::types::{CompletionResponse, Message};
use crate::limits::ResourceLimits;

// ---------------------------------------------------------------------------
// LlmProvider trait
// ---------------------------------------------------------------------------

/// Callback interface that the host supplies so WASM modules can call the
/// chatty LLM back-end.
///
/// The implementation is called synchronously, on a helper thread that has
/// the caller's Tokio runtime entered (so `Handle::current().block_on` works
/// for an async client). The host stops waiting when the guest call's
/// wall-clock deadline passes and hands the guest `Err("deadline exceeded")`;
/// the helper thread is left to finish on its own.
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
/// Holds the resource limiter (for memory caps), the current call's
/// deadline, and the runtime dependencies needed by host imports.
pub(crate) struct ModuleState {
    /// Wasmtime resource limiter (memory cap).
    pub(crate) limiter: GuestLimiter,
    /// Wall-clock deadline of the guest call in progress, if any. Host
    /// imports that can block stop waiting at this instant.
    pub(crate) deadline: Option<Instant>,
    /// Set when a host import gave up because `deadline` passed.
    pub(crate) deadline_hit: bool,
    /// The last few KiB the guest wrote to stderr (e.g. a panic message).
    pub(crate) stderr: StderrTail,
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
        let limiter = GuestLimiter::new(resource_limits.max_memory_bytes);

        // Minimal WASI context — no filesystem, no network, no env vars.
        // Modules compiled for wasm32-wasip2 import these interfaces from
        // the host; we satisfy them with a sandboxed no-op implementation.
        // Only stderr is kept (its tail), so a guest panic message can be
        // reported with the trap.
        let stderr = StderrTail::default();
        let wasi_ctx = WasiCtxBuilder::new().stderr(stderr.clone()).build();
        let table = ResourceTable::new();

        Self {
            limiter,
            deadline: None,
            deadline_hit: false,
            stderr,
            manifest,
            llm_provider,
            billing_provider,
            wasi_ctx,
            table,
            progress_tx: None,
        }
    }

    /// Run a blocking host operation, giving up when the current call's
    /// deadline passes.
    ///
    /// `f` runs on a helper thread with the caller's Tokio runtime entered.
    /// On expiry the guest gets `Err("deadline exceeded")` and
    /// `deadline_hit` is set; the helper thread finishes on its own. A panic
    /// in `f` becomes an `Err` too, never a host panic.
    fn before_deadline<T: Send + 'static>(
        &mut self,
        what: &str,
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let Some(deadline) = self.deadline else {
            return f();
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.deadline_hit = true;
            return Err(DEADLINE_EXCEEDED.to_string());
        }
        let (tx, rx) = sync_channel(1);
        let runtime = tokio::runtime::Handle::try_current().ok();
        std::thread::Builder::new()
            .name(format!("wasm-host-{what}"))
            .spawn(move || {
                let _entered = runtime.as_ref().map(|h| h.enter());
                let _ = tx.send(f());
            })
            .map_err(|e| format!("{what}: could not start the host call: {e}"))?;
        match rx.recv_timeout(remaining) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.deadline_hit = true;
                warn!(module = %self.manifest.name, what, "host call ran past the call deadline");
                Err(DEADLINE_EXCEEDED.to_string())
            }
            Err(RecvTimeoutError::Disconnected) => Err(format!("{what}: the host call panicked")),
        }
    }
}

/// What a host import returns to the guest when the call's deadline passed.
const DEADLINE_EXCEEDED: &str = "deadline exceeded";

// ---------------------------------------------------------------------------
// GuestLimiter — memory cap that remembers refusing a grow
// ---------------------------------------------------------------------------

/// [`StoreLimits`] plus a flag set whenever a memory grow is refused, so a
/// trap that follows can be reported as `memory limit`.
pub(crate) struct GuestLimiter {
    inner: StoreLimits,
    /// A memory grow was refused during the current call.
    pub(crate) memory_denied: bool,
}

impl GuestLimiter {
    fn new(max_memory_bytes: u64) -> Self {
        let inner = wasmtime::StoreLimitsBuilder::new()
            .memory_size(usize::try_from(max_memory_bytes).unwrap_or(usize::MAX))
            .build();
        Self {
            inner,
            memory_denied: false,
        }
    }
}

impl ResourceLimiter for GuestLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        let allowed = self.inner.memory_growing(current, desired, maximum)?;
        if !allowed {
            self.memory_denied = true;
        }
        Ok(allowed)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        self.inner.table_growing(current, desired, maximum)
    }

    fn instances(&self) -> usize {
        self.inner.instances()
    }

    fn tables(&self) -> usize {
        self.inner.tables()
    }

    fn memories(&self) -> usize {
        self.inner.memories()
    }
}

// ---------------------------------------------------------------------------
// StderrTail — the last few KiB of guest stderr
// ---------------------------------------------------------------------------

/// Bytes of guest stderr kept for error reports.
const STDERR_TAIL_BYTES: usize = 4096;

/// A WASI stderr that keeps only its last [`STDERR_TAIL_BYTES`] bytes and
/// never fails a write, so a chatty guest can't trap itself through stderr.
#[derive(Clone, Default)]
pub(crate) struct StderrTail(Arc<Mutex<VecDeque<u8>>>);

impl StderrTail {
    /// Forget everything written so far.
    pub(crate) fn clear(&self) {
        self.0.lock().expect("stderr tail lock").clear();
    }

    /// What is kept, lossily decoded and trimmed.
    pub(crate) fn text(&self) -> String {
        let buf = self.0.lock().expect("stderr tail lock");
        let (a, b) = buf.as_slices();
        String::from_utf8_lossy(&[a, b].concat()).trim().to_string()
    }
}

impl OutputStream for StderrTail {
    fn write(&mut self, bytes: bytes::Bytes) -> Result<(), StreamError> {
        let mut buf = self.0.lock().expect("stderr tail lock");
        let keep = bytes.len().min(STDERR_TAIL_BYTES);
        buf.extend(&bytes[bytes.len() - keep..]);
        let excess = buf.len().saturating_sub(STDERR_TAIL_BYTES);
        buf.drain(..excess);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StreamError> {
        Ok(())
    }

    fn check_write(&mut self) -> Result<usize, StreamError> {
        Ok(usize::MAX)
    }
}

#[wasmtime_wasi::async_trait]
impl Pollable for StderrTail {
    async fn ready(&mut self) {}
}

impl StdoutStream for StderrTail {
    fn stream(&self) -> Box<dyn OutputStream> {
        Box::new(self.clone())
    }

    fn isatty(&self) -> bool {
        false
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
        let provider = Arc::clone(&self.llm_provider);
        let result = self.before_deadline("llm-complete", move || {
            provider.complete(&model, messages, tools)
        });
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

        match self.billing_provider.clone() {
            Some(provider) => {
                let result = self.before_deadline("billing-acquire-session", move || {
                    provider.acquire_session(estimated_tokens)
                });
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

        match self.billing_provider.clone() {
            Some(provider) => {
                let result = self.before_deadline("billing-report-usage", move || {
                    provider.report_usage(input_tokens, output_tokens)
                });
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

        let result = self.before_deadline("file-read-bytes", {
            let full = full.clone();
            move || std::fs::read(&full).map_err(|e| format!("file::read_bytes: {e}"))
        });
        if let Err(ref e) = result {
            warn!(module = %self.manifest.name, path = %full.display(), error = %e, "file::read_bytes failed");
        }
        result
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

    struct SlowProvider(std::time::Duration);

    impl LlmProvider for SlowProvider {
        fn complete(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _tools: Option<String>,
        ) -> Result<CompletionResponse, String> {
            std::thread::sleep(self.0);
            Err("too late to matter".to_string())
        }
    }

    struct PanickingProvider;

    impl LlmProvider for PanickingProvider {
        fn complete(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _tools: Option<String>,
        ) -> Result<CompletionResponse, String> {
            panic!("provider bug")
        }
    }

    #[test]
    fn llm_host_stops_waiting_at_the_call_deadline() {
        use crate::bindings::chatty::module::llm::Host;
        let slow = std::time::Duration::from_secs(5);
        let mut state = make_state(Arc::new(SlowProvider(slow)));
        state.deadline = Some(Instant::now() + std::time::Duration::from_millis(100));

        let start = Instant::now();
        let result = state.complete("m".to_string(), vec![], None);
        assert_eq!(result.unwrap_err(), DEADLINE_EXCEEDED);
        assert!(state.deadline_hit);
        assert!(start.elapsed() < slow, "waited {:?}", start.elapsed());

        // Past the deadline, the host doesn't start the call at all.
        let result = state.complete("m".to_string(), vec![], None);
        assert_eq!(result.unwrap_err(), DEADLINE_EXCEEDED);
    }

    #[test]
    fn llm_host_maps_a_provider_panic_to_an_error() {
        use crate::bindings::chatty::module::llm::Host;
        let mut state = make_state(Arc::new(PanickingProvider));
        state.deadline = Some(Instant::now() + std::time::Duration::from_secs(5));
        let err = state.complete("m".to_string(), vec![], None).unwrap_err();
        assert!(err.contains("panicked"), "{err}");
        assert!(!state.deadline_hit);
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
