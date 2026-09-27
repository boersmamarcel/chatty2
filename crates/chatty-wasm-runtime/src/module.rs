use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, EngineWeak, Store, Trap};

use crate::bindings::Module;
use crate::bindings::chatty::module::types::{
    AgentCard, ChatRequest, ChatResponse, ToolDefinition,
};
use crate::error::CallError;
use crate::host::{BillingProvider, LlmProvider, ModuleManifest, ModuleState};
use crate::limits::{EPOCH_TICK, METADATA_CALL_MS, ResourceLimits};

// ---------------------------------------------------------------------------
// Epoch ticker
// ---------------------------------------------------------------------------

/// Every engine built by [`WasmModule::build_engine`] that is still alive.
/// `None` until the ticker thread has been started.
static TICKED_ENGINES: Mutex<Option<Vec<EngineWeak>>> = Mutex::new(None);

/// Add `engine` to the process-wide epoch ticker, starting the ticker thread
/// on first use. The thread advances every live engine's epoch once per
/// [`EPOCH_TICK`]; that is what makes a store's epoch deadline fire. Engines
/// are held weakly, so a dropped engine falls out of the list.
fn tick_epochs_of(engine: &Engine) -> Result<()> {
    let mut engines = TICKED_ENGINES.lock().unwrap_or_else(|e| e.into_inner());
    let engines = match &mut *engines {
        Some(engines) => engines,
        None => {
            std::thread::Builder::new()
                .name("wasm-epoch-ticker".into())
                .spawn(|| {
                    loop {
                        std::thread::sleep(EPOCH_TICK);
                        let mut engines = TICKED_ENGINES.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(engines) = engines.as_mut() {
                            engines.retain(|weak| match weak.upgrade() {
                                Some(engine) => {
                                    engine.increment_epoch();
                                    true
                                }
                                None => false,
                            });
                        }
                    }
                })
                .context("failed to start the WASM epoch ticker thread")?;
            engines.insert(Vec::new())
        }
    };
    engines.push(engine.weak());
    Ok(())
}

// ---------------------------------------------------------------------------
// WasmModule
// ---------------------------------------------------------------------------

/// Metrics captured from the last module invocation.
#[derive(Debug, Clone, Default)]
pub struct InvocationMetrics {
    pub execution_ms: u32,
    /// Fuel the call consumed, counted from its own per-call budget.
    pub fuel_consumed: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

/// A live component instance: the store and the typed export bindings.
struct Instance {
    store: Store<ModuleState>,
    bindings: Module,
}

/// A loaded WASM component module.
///
/// Every export call runs under the per-call [`ResourceLimits`]: fuel is
/// refilled and an epoch deadline is set before each call, and the call runs
/// off the async executor (`spawn_blocking` for `chat` / `invoke_tool`, a
/// scoped thread for the metadata exports), so a guest trap, panic or memory
/// failure comes back as a [`CallError`] and never takes the host down.
///
/// A call that traps drops its instance; the next call instantiates the
/// component afresh (guest state such as statics starts over).
pub struct WasmModule {
    engine: Engine,
    component: Component,
    linker: Linker<ModuleState>,
    manifest: ModuleManifest,
    llm_provider: Arc<dyn LlmProvider>,
    billing_provider: Option<Arc<dyn BillingProvider>>,
    /// Per-call limits.
    limits: ResourceLimits,
    /// `None` after a trap, until the next call re-instantiates.
    instance: Option<Instance>,
    /// Progress channel for the next `chat` (see [`Self::set_progress_sender`]).
    progress_tx: Option<UnboundedSender<String>>,
    /// Metrics from the most recent invocation.
    last_metrics: Option<InvocationMetrics>,
}

impl WasmModule {
    /// Build a Wasmtime [`Engine`] pre-configured for the component model,
    /// fuel metering and epoch interruption, and register it with the
    /// process-wide epoch ticker that drives wall-clock deadlines.
    ///
    /// Memory limits are enforced per store (see `ModuleState`); limits
    /// are not engine settings, so callers may share one engine across
    /// modules with different limits.
    pub fn build_engine(_limits: &ResourceLimits) -> Result<Engine> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        // SIMD128: 4-wide f32/i32 vector ops — ~4× throughput for ML kernels.
        // Enabled here; modules opt in at compile time via +simd128 RUSTFLAGS.
        config.wasm_simd(true);
        // Threads proposal: shared linear memory + atomics for Rayon-based
        // parallelism in ML modules (wasm32-wasip2 + wasi:threads).
        config.wasm_threads(true);

        let engine = Engine::new(&config).context("failed to create Wasmtime engine")?;
        tick_epochs_of(&engine)?;
        Ok(engine)
    }

    /// Load a WASM component from `path` and instantiate it.
    ///
    /// # Arguments
    /// * `engine`       — an engine from [`Self::build_engine`]
    /// * `path`         — path to a `.wasm` component file
    /// * `manifest`     — module metadata and config values
    /// * `llm_provider` — host callback for LLM completions
    /// * `limits`       — per-call resource caps, used as given (the
    ///   registry clamps manifest-supplied values to the host ceilings)
    pub fn from_file(
        engine: &Engine,
        path: &Path,
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        Self::from_file_with_billing(engine, path, manifest, llm_provider, None, limits)
    }

    /// Load a WASM component from `path` with optional billing provider.
    pub fn from_file_with_billing(
        engine: &Engine,
        path: &Path,
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        billing_provider: Option<Arc<dyn BillingProvider>>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        info!(path = %path.display(), module = %manifest.name, "loading WASM module");

        let component =
            Component::from_file(engine, path).context("failed to load WASM component")?;

        Self::from_component(
            engine,
            component,
            manifest,
            llm_provider,
            billing_provider,
            limits,
        )
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
        Self::from_bytes_with_billing(engine, bytes, manifest, llm_provider, None, limits)
    }

    /// Load a WASM component from raw bytes with optional billing provider.
    pub fn from_bytes_with_billing(
        engine: &Engine,
        bytes: &[u8],
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        billing_provider: Option<Arc<dyn BillingProvider>>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        let component =
            Component::from_binary(engine, bytes).context("failed to parse WASM component")?;

        Self::from_component(
            engine,
            component,
            manifest,
            llm_provider,
            billing_provider,
            limits,
        )
    }

    fn from_component(
        engine: &Engine,
        component: Component,
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        billing_provider: Option<Arc<dyn BillingProvider>>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        let mut linker: Linker<ModuleState> = Linker::new(engine);

        // Add WASI Preview 2 host implementations first — modules compiled
        // for wasm32-wasip2 import WASI interfaces (e.g. wasi:io/poll) from
        // the host even when they don't actively use them.
        wasmtime_wasi::add_to_linker_sync(&mut linker).context("failed to add WASI to linker")?;

        Module::add_to_linker(&mut linker, |state| state)
            .context("failed to add host imports to linker")?;

        let mut module = Self {
            engine: engine.clone(),
            component,
            linker,
            manifest,
            llm_provider,
            billing_provider,
            limits,
            instance: None,
            progress_tx: None,
            last_metrics: None,
        };
        // Instantiate now so a module that can't instantiate fails to load.
        module.instance = Some(module.instantiate()?);
        Ok(module)
    }

    /// Create a fresh store and instance of the component.
    fn instantiate(&self) -> Result<Instance> {
        let state = ModuleState::new(
            self.manifest.clone(),
            Arc::clone(&self.llm_provider),
            self.billing_provider.clone(),
            &self.limits,
        );
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limiter);
        // Instantiation may run guest code: bound it like a call.
        store
            .set_fuel(self.limits.max_fuel)
            .context("failed to set fuel")?;
        store.set_epoch_deadline(epoch_ticks(self.limits.max_execution_ms));

        let bindings = Module::instantiate(&mut store, &self.component, &self.linker)
            .context("failed to instantiate WASM module")?;
        debug!(module = %self.manifest.name, "WASM module instantiated (chatty:module@0.2.0)");
        Ok(Instance { store, bindings })
    }

    /// The live instance, or a fresh one if the last call trapped.
    fn take_instance(&mut self) -> Result<Instance> {
        match self.instance.take() {
            Some(instance) => Ok(instance),
            None => {
                debug!(module = %self.manifest.name, "re-instantiating after a trap");
                self.instantiate()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Progress channel
    // -----------------------------------------------------------------------

    /// Install a progress sender so module log messages are forwarded as
    /// real-time progress events during the next `chat()`.
    pub fn set_progress_sender(&mut self, tx: UnboundedSender<String>) {
        self.progress_tx = Some(tx);
    }

    // -----------------------------------------------------------------------
    // Guest export wrappers
    // -----------------------------------------------------------------------

    /// Call the `agent::chat` export under the per-call limits.
    ///
    /// Returns the chat response, the guest's own error, or a [`CallError`]
    /// (fuel, deadline, memory, trap, output size).
    /// Call [`Self::last_invocation_metrics`] after this to get execution metrics.
    pub async fn chat(&mut self, req: ChatRequest) -> Result<ChatResponse> {
        let result = self
            .call_blocking(
                "chat",
                move |bindings, store| bindings.chatty_module_agent().call_chat(store, &req),
                chat_response_size,
            )
            .await;
        // The progress sender belongs to this one chat.
        self.progress_tx = None;

        if let (Ok(resp), Some(metrics)) = (&result, self.last_metrics.as_mut())
            && let Some(usage) = &resp.usage
        {
            metrics.input_tokens = Some(usage.input_tokens);
            metrics.output_tokens = Some(usage.output_tokens);
        }
        result
    }

    /// Call the `agent::invoke-tool` export under the per-call limits.
    pub async fn invoke_tool(&mut self, name: &str, args: &str) -> Result<String> {
        let (name, args) = (name.to_string(), args.to_string());
        self.call_blocking(
            "invoke-tool",
            move |bindings, store| {
                bindings
                    .chatty_module_agent()
                    .call_invoke_tool(store, &name, &args)
            },
            String::len,
        )
        .await
    }

    /// Call the `agent::list-tools` export (budget: [`METADATA_CALL_MS`]).
    pub fn list_tools(&mut self) -> Result<Vec<ToolDefinition>> {
        self.call_metadata(
            "list-tools",
            |bindings, store| {
                bindings
                    .chatty_module_agent()
                    .call_list_tools(store)
                    .map(Ok)
            },
            |tools| tools.iter().map(tool_definition_size).sum(),
        )
    }

    /// Call the `agent::get-agent-card` export (budget: [`METADATA_CALL_MS`]).
    pub fn agent_card(&mut self) -> Result<AgentCard> {
        self.call_metadata(
            "get-agent-card",
            |bindings, store| {
                bindings
                    .chatty_module_agent()
                    .call_get_agent_card(store)
                    .map(Ok)
            },
            agent_card_size,
        )
    }

    /// Fuel left in the store after the most recent call (each call starts
    /// from the full per-call budget). Useful for diagnostics and tests.
    pub fn remaining_fuel(&self) -> u64 {
        self.instance
            .as_ref()
            .and_then(|i| i.store.get_fuel().ok())
            .unwrap_or(0)
    }

    /// Get the metrics from the most recent invocation (chat or invoke_tool).
    pub fn last_invocation_metrics(&self) -> Option<InvocationMetrics> {
        self.last_metrics.clone()
    }

    /// Run `call` on the blocking pool under the full per-call limits, and
    /// record [`InvocationMetrics`] for it.
    async fn call_blocking<O: Send + 'static>(
        &mut self,
        export: &'static str,
        call: impl FnOnce(&Module, &mut Store<ModuleState>) -> wasmtime::Result<Result<O, String>>
        + Send
        + 'static,
        size: fn(&O) -> usize,
    ) -> Result<O> {
        let mut instance = self.take_instance()?;
        let limits = self.limits.clone();
        let progress_tx = self.progress_tx.clone();
        let start = Instant::now();

        let joined = tokio::task::spawn_blocking(move || {
            let budget = limits.max_execution_ms;
            let report = run_export(&mut instance, &limits, budget, progress_tx, call, size);
            (instance, report)
        })
        .await;

        let (result, fuel_consumed) = match joined {
            Ok((instance, report)) => {
                if !report.trapped {
                    self.instance = Some(instance);
                }
                (report.result, report.fuel_consumed)
            }
            Err(join) => (Err(host_panic(join.to_string())), 0),
        };
        self.last_metrics = Some(InvocationMetrics {
            execution_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
            fuel_consumed,
            input_tokens: None,
            output_tokens: None,
        });
        finish(export, result)
    }

    /// Run a metadata export on a scoped thread (off any async executor)
    /// with a [`METADATA_CALL_MS`] budget.
    fn call_metadata<O: Send>(
        &mut self,
        export: &'static str,
        call: impl FnOnce(&Module, &mut Store<ModuleState>) -> wasmtime::Result<Result<O, String>>
        + Send,
        size: fn(&O) -> usize,
    ) -> Result<O> {
        let mut instance = self.take_instance()?;
        let limits = &self.limits;
        let budget = METADATA_CALL_MS.min(limits.max_execution_ms);

        let joined = std::thread::scope(|scope| {
            scope
                .spawn(|| run_export(&mut instance, limits, budget, None, call, size))
                .join()
        });
        let result = match joined {
            Ok(report) => {
                if !report.trapped {
                    self.instance = Some(instance);
                }
                report.result
            }
            Err(panic) => Err(host_panic(panic_message(&panic))),
        };
        finish(export, result)
    }
}

/// Flatten a call's outcome into the wrapper's `Result`.
fn finish<O>(export: &str, result: Result<Result<O, String>>) -> Result<O> {
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(anyhow::anyhow!("agent::{export} returned error: {message}")),
        Err(e) => Err(e.context(format!("agent::{export} failed"))),
    }
}

// ---------------------------------------------------------------------------
// One export call under the limits
// ---------------------------------------------------------------------------

/// What one export call produced.
struct CallReport<O> {
    /// The guest's own `Ok`/`Err`, or why the call failed.
    result: Result<Result<O, String>>,
    /// Fuel used, from this call's own budget.
    fuel_consumed: u64,
    /// The call trapped; the instance must not be entered again.
    trapped: bool,
}

/// Epoch ticks covering `ms` milliseconds (at least one).
fn epoch_ticks(ms: u64) -> u64 {
    ms.div_ceil(EPOCH_TICK.as_millis() as u64).max(1)
}

/// Refill fuel, arm the epoch deadline and the host-time deadline, run
/// `call`, then map how it ended. Runs on whatever thread calls it; callers
/// keep it off async executor threads (WASI's sync bindings `block_on`).
fn run_export<O>(
    instance: &mut Instance,
    limits: &ResourceLimits,
    budget_ms: u64,
    progress_tx: Option<UnboundedSender<String>>,
    call: impl FnOnce(&Module, &mut Store<ModuleState>) -> wasmtime::Result<Result<O, String>>,
    size: fn(&O) -> usize,
) -> CallReport<O> {
    let store = &mut instance.store;
    if let Err(e) = store.set_fuel(limits.max_fuel) {
        return CallReport {
            result: Err(e.context("failed to set fuel")),
            fuel_consumed: 0,
            trapped: false,
        };
    }
    store.set_epoch_deadline(epoch_ticks(budget_ms));
    {
        let state = store.data_mut();
        state.deadline = Some(Instant::now() + Duration::from_millis(budget_ms));
        state.deadline_hit = false;
        state.limiter.memory_denied = false;
        state.stderr.clear();
        state.progress_tx = progress_tx;
    }

    let outcome = call(&instance.bindings, store);

    let fuel_consumed = limits
        .max_fuel
        .saturating_sub(store.get_fuel().unwrap_or(0));
    let state = store.data_mut();
    state.deadline = None;
    state.progress_tx = None;
    let deadline = CallError::DeadlineExceeded {
        max_execution_ms: budget_ms,
    };

    let (result, trapped) = match outcome {
        Err(err) => (Err(classify_trap(&err, state, limits, budget_ms)), true),
        Ok(_) if state.deadline_hit => (Err(deadline), false),
        Ok(returned) => {
            let bytes = match &returned {
                Ok(value) => size(value),
                Err(message) => message.len(),
            };
            let result = if bytes as u64 > limits.max_output_bytes {
                Err(CallError::OutputTooLarge {
                    bytes,
                    max_output_bytes: limits.max_output_bytes,
                })
            } else {
                Ok(returned)
            };
            (result, false)
        }
    };
    let result = result.map_err(|call_error| {
        warn!(module = %state.manifest.name, error = %call_error, "guest call failed");
        anyhow::Error::new(call_error)
    });
    CallReport {
        result,
        fuel_consumed,
        trapped,
    }
}

/// Name why a call trapped: fuel, deadline, memory limit, or the guest's own
/// trap (with the tail of its stderr, where a panic message lands).
fn classify_trap(
    err: &wasmtime::Error,
    state: &ModuleState,
    limits: &ResourceLimits,
    budget_ms: u64,
) -> CallError {
    match err.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => CallError::FuelExhausted {
            max_fuel: limits.max_fuel,
        },
        Some(Trap::Interrupt) => CallError::DeadlineExceeded {
            max_execution_ms: budget_ms,
        },
        _ if state.limiter.memory_denied => CallError::MemoryLimit {
            max_memory_bytes: limits.max_memory_bytes,
        },
        _ => {
            let cause = err.root_cause().to_string();
            let stderr = state.stderr.text();
            CallError::GuestTrap(if stderr.is_empty() {
                cause
            } else {
                format!("{cause}; guest stderr: {stderr}")
            })
        }
    }
}

fn host_panic(message: String) -> anyhow::Error {
    anyhow::Error::new(CallError::HostPanic(message))
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

// ---------------------------------------------------------------------------
// Output sizes (for the per-call output cap)
// ---------------------------------------------------------------------------

fn chat_response_size(resp: &ChatResponse) -> usize {
    resp.content.len()
        + resp
            .tool_calls
            .iter()
            .map(|c| c.id.len() + c.name.len() + c.arguments.len())
            .sum::<usize>()
}

fn tool_definition_size(tool: &ToolDefinition) -> usize {
    tool.name.len() + tool.description.len() + tool.parameters_schema.len()
}

fn agent_card_size(card: &AgentCard) -> usize {
    card.name.len()
        + card.display_name.len()
        + card.description.len()
        + card.version.len()
        + card
            .skills
            .iter()
            .map(|s| {
                s.name.len()
                    + s.description.len()
                    + s.examples.iter().map(String::len).sum::<usize>()
            })
            .sum::<usize>()
        + card.tools.iter().map(tool_definition_size).sum::<usize>()
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
