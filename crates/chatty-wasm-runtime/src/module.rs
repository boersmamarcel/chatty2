use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
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
}

impl WasmModule {
    /// Build a Wasmtime [`Engine`] pre-configured for the component model
    /// and fuel metering.
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
        })
    }

    // -----------------------------------------------------------------------
    // Guest export wrappers
    // -----------------------------------------------------------------------

    /// Call the `agent::chat` export with a timeout.
    ///
    /// Returns the chat response or a descriptive error if the module traps,
    /// runs out of fuel, or exceeds the wall-clock timeout.
    pub async fn chat(&mut self, req: ChatRequest) -> Result<ChatResponse> {
        let timeout = Duration::from_millis(self.limits.max_execution_ms);
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
    }

    /// Call the `agent::invoke-tool` export with a timeout.
    pub async fn invoke_tool(&mut self, name: &str, args: &str) -> Result<String> {
        let timeout = Duration::from_millis(self.limits.max_execution_ms);
        let agent = self.module.chatty_module_agent();
        let store = &mut self.store;

        tokio::time::timeout(timeout, async move {
            agent
                .call_invoke_tool(store, name, args)
                .context("WASM trap in agent::invoke-tool")?
                .map_err(|e| anyhow::anyhow!("agent::invoke-tool returned error: {e}"))
        })
        .await
        .context("agent::invoke-tool timed out")?
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
