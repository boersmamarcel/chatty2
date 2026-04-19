use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

use crate::bindings::Module;
use crate::bindings::chatty::module::types::{
    AgentCard, ChatRequest, ChatResponse, ToolDefinition,
};
use crate::host::{LlmProvider, ModuleManifest, ModuleState};
use crate::limits::ResourceLimits;

// ---------------------------------------------------------------------------
// WasmModule
// ---------------------------------------------------------------------------

/// Metrics captured from the last module invocation.
#[derive(Debug, Clone, Default)]
pub struct InvocationMetrics {
    pub execution_ms: u32,
    pub fuel_consumed: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

/// A loaded and instantiated WASM component module.
///
/// Wraps a Wasmtime [`Store`] and the generated [`Module`] binding so the
/// host can call guest exports through a typed Rust API.
pub struct WasmModule {
    /// Wasmtime store holding the instance state.
    store: Store<ModuleState>,
    /// Generated world wrapper giving typed access to guest exports.
    module: Module,
    /// Resource limits — kept for timeout enforcement.
    limits: ResourceLimits,
    /// Metrics from the most recent invocation.
    last_metrics: Option<InvocationMetrics>,
}

impl WasmModule {
    /// Build a Wasmtime [`Engine`] pre-configured for the component model
    /// and fuel metering.
    ///
    /// Memory limits are enforced at runtime via [`StoreLimitsBuilder`] in
    /// `from_component`; only fuel and component model are set on the engine.
    ///
    /// Callers may share one engine across multiple modules.
    pub fn build_engine(_limits: &ResourceLimits) -> Result<Engine> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);

        Engine::new(&config).context("failed to create Wasmtime engine")
    }

    /// Load a WASM component from `path` and instantiate it.
    ///
    /// # Arguments
    /// * `engine`       — shared Wasmtime engine (must have `component-model` and `consume_fuel`)
    /// * `path`         — path to a `.wasm` component file
    /// * `manifest`     — module metadata and config values
    /// * `llm_provider` — host callback for LLM completions
    /// * `limits`       — resource caps (fuel, memory, timeout)
    pub fn from_file(
        engine: &Engine,
        path: &Path,
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        info!(path = %path.display(), module = %manifest.name, "loading WASM module");

        let component =
            Component::from_file(engine, path).context("failed to load WASM component")?;

        Self::from_component(engine, &component, manifest, llm_provider, limits)
    }

    /// Load a WASM component from raw bytes and instantiate it.
    ///
    /// Useful in tests where the binary is embedded at compile time.
    pub fn from_bytes(
        engine: &Engine,
        bytes: &[u8],
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        let component =
            Component::from_binary(engine, bytes).context("failed to parse WASM component")?;

        Self::from_component(engine, &component, manifest, llm_provider, limits)
    }

    fn from_component(
        engine: &Engine,
        component: &Component,
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        let mut linker: Linker<ModuleState> = Linker::new(engine);

        // Add WASI Preview 2 host implementations first — modules compiled
        // for wasm32-wasip2 import WASI interfaces (e.g. wasi:io/poll) from
        // the host even when they don't actively use them.
        wasmtime_wasi::add_to_linker_sync(&mut linker).context("failed to add WASI to linker")?;

        Module::add_to_linker(&mut linker, |state| state)
            .context("failed to add host imports to linker")?;

        let state = ModuleState::new(manifest, llm_provider, &limits);
        let mut store = Store::new(engine, state);

        // Register memory limiter.
        store.limiter(|s| &mut s.limits);

        // Set initial fuel.
        store
            .set_fuel(limits.max_fuel)
            .context("failed to set fuel")?;

        let module = Module::instantiate(&mut store, component, &linker)
            .context("failed to instantiate WASM module")?;

        debug!("WASM module instantiated successfully");

        Ok(Self {
            store,
            module,
            limits,
            last_metrics: None,
        })
    }

    // -----------------------------------------------------------------------
    // Progress channel
    // -----------------------------------------------------------------------

    /// Install a progress sender so module log messages are forwarded as
    /// real-time progress events during `chat()`.
    pub fn set_progress_sender(&mut self, tx: UnboundedSender<String>) {
        self.store.data_mut().progress_tx = Some(tx);
    }

    // -----------------------------------------------------------------------
    // Guest export wrappers
    // -----------------------------------------------------------------------

    /// Call the `agent::chat` export with a timeout.
    ///
    /// Returns the chat response or a descriptive error if the module traps,
    /// runs out of fuel, or exceeds the wall-clock timeout.
    /// Call [`last_invocation_metrics`] after this to get execution metrics.
    pub async fn chat(&mut self, req: ChatRequest) -> Result<ChatResponse> {
        let timeout = Duration::from_millis(self.limits.max_execution_ms);
        let start_time = std::time::Instant::now();
        let initial_fuel = self.store.get_fuel().unwrap_or(0);

        let result = {
            let agent = self.module.chatty_module_agent();
            let store = &mut self.store;

            tokio::time::timeout(timeout, async move {
                agent
                    .call_chat(store, &req)
                    .context("WASM trap in agent::chat")?
                    .map_err(|e| anyhow::anyhow!("agent::chat returned error: {e}"))
            })
            .await
            .context("agent::chat timed out")?
        };

        // Capture metrics
        let execution_ms = start_time.elapsed().as_millis() as u32;
        let remaining_fuel = self.store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(remaining_fuel);

        let (input_tokens, output_tokens) = match &result {
            Ok(resp) => resp
                .usage
                .as_ref()
                .map(|u| (Some(u.input_tokens), Some(u.output_tokens)))
                .unwrap_or_default(),
            Err(_) => (None, None),
        };

        self.last_metrics = Some(InvocationMetrics {
            execution_ms,
            fuel_consumed,
            input_tokens,
            output_tokens,
        });

        // Clear progress sender after chat completes
        self.store.data_mut().progress_tx = None;

        result
    }

    /// Call the `agent::invoke-tool` export with a timeout.
    pub async fn invoke_tool(&mut self, name: &str, args: &str) -> Result<String> {
        let timeout = Duration::from_millis(self.limits.max_execution_ms);
        let start_time = std::time::Instant::now();
        let initial_fuel = self.store.get_fuel().unwrap_or(0);

        let agent = self.module.chatty_module_agent();
        let store = &mut self.store;

        let result = tokio::time::timeout(timeout, async move {
            agent
                .call_invoke_tool(store, name, args)
                .context("WASM trap in agent::invoke-tool")?
                .map_err(|e| anyhow::anyhow!("agent::invoke-tool returned error: {e}"))
        })
        .await
        .context("agent::invoke-tool timed out")?;

        // Capture metrics
        let execution_ms = start_time.elapsed().as_millis() as u32;
        let remaining_fuel = self.store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(remaining_fuel);

        self.last_metrics = Some(InvocationMetrics {
            execution_ms,
            fuel_consumed,
            input_tokens: None,
            output_tokens: None,
        });

        result
    }

    /// Call the `agent::list-tools` export.
    pub fn list_tools(&mut self) -> Result<Vec<ToolDefinition>> {
        self.module
            .chatty_module_agent()
            .call_list_tools(&mut self.store)
            .context("WASM trap in agent::list-tools")
    }

    /// Call the `agent::get-agent-card` export.
    pub fn agent_card(&mut self) -> Result<AgentCard> {
        self.module
            .chatty_module_agent()
            .call_get_agent_card(&mut self.store)
            .context("WASM trap in agent::get-agent-card")
    }

    /// Return the remaining fuel in the store.
    ///
    /// Useful for diagnostics and tests.
    pub fn remaining_fuel(&self) -> u64 {
        self.store.get_fuel().unwrap_or(0)
    }

    /// Get the metrics from the most recent invocation (chat or invoke_tool).
    pub fn last_invocation_metrics(&self) -> Option<InvocationMetrics> {
        self.last_metrics.clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::chatty::module::types::{CompletionResponse, Message};

    struct MockProvider;

    impl LlmProvider for MockProvider {
        fn complete(
            &self,
            _model: &str,
            messages: Vec<Message>,
            _tools: Option<String>,
        ) -> Result<CompletionResponse, String> {
            let last = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            Ok(CompletionResponse {
                content: format!("mock: {last}"),
                tool_calls: vec![],
                usage: None,
            })
        }
    }

    #[test]
    fn build_engine_succeeds() {
        let limits = ResourceLimits::default();
        let engine = WasmModule::build_engine(&limits);
        assert!(engine.is_ok(), "build_engine should not fail: {:?}", engine);
    }

    #[test]
    fn from_file_nonexistent_path_gives_error() {
        let limits = ResourceLimits::default();
        let engine = WasmModule::build_engine(&limits).unwrap();
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider);
        let result = WasmModule::from_file(
            &engine,
            Path::new("/nonexistent/path/module.wasm"),
            ModuleManifest::new("test"),
            provider,
            limits,
        );
        assert!(result.is_err());
    }

    #[test]
    fn from_bytes_invalid_wasm_gives_error() {
        let limits = ResourceLimits::default();
        let engine = WasmModule::build_engine(&limits).unwrap();
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider);
        let result = WasmModule::from_bytes(
            &engine,
            b"not valid wasm",
            ModuleManifest::new("test"),
            provider,
            limits,
        );
        assert!(result.is_err());
    }
}
