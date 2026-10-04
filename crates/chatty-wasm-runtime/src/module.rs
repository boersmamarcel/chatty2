use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use std::convert::Infallible;
use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context, Result};
use tokio::runtime::RuntimeFlavor;
use tracing::{debug, info, warn};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, EngineWeak, Store, Trap};

use crate::bindings::PluginWorld;
use crate::bindings::chatty::plugin::types::ToolDefinition;
use crate::bindings::exports::chatty::plugin::plugin::Capability;
use crate::bindings::exports::chatty::plugin::plugin::{
    PluginMetadata, ToolCallRequest, ToolResult,
};
use crate::error::{CallError, ToolFailure, UnsupportedWorld};
use crate::grants::{self, NotGranted};
use crate::host::{
    LlmProvider, ModuleManifest, ModuleState, add_deadline_clock_to_linker,
};
use crate::limits::{EPOCH_TICK, MAX_OUTPUT_BYTES_CEILING, METADATA_CALL_MS, ResourceLimits};

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
    bindings: PluginWorld,
}

/// A loaded WASM plugin: a component exporting `chatty:plugin/plugin@0.4.0`.
///
/// Every export call runs under the per-call [`ResourceLimits`]: fuel is
/// refilled and an epoch deadline is set before each call, and the call runs
/// off the async executor (`block_in_place` or `spawn_blocking` for
/// `invoke_tool`, see `call_blocking`; a scoped thread for the metadata
/// exports), so a guest trap, panic or memory failure comes back as a
/// [`CallError`] and never takes the host down.
///
/// A call that traps drops its instance; the next call instantiates the
/// component afresh (guest state such as statics starts over).
pub struct WasmModule {
    engine: Engine,
    component: Component,
    linker: Linker<ModuleState>,
    manifest: ModuleManifest,
    llm_provider: Arc<dyn LlmProvider>,
    /// Per-call limits.
    limits: ResourceLimits,
    /// What the plugin's `metadata` requests.
    requested: Vec<Capability>,
    /// What it is linked against (always including `logging`).
    granted: Vec<Capability>,
    /// `None` after a trap, until the next call re-instantiates.
    instance: Option<Instance>,
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

        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("failed to create Wasmtime engine")?;
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
        info!(path = %path.display(), module = %manifest.name, "loading WASM module");

        let component = Component::from_file(engine, path)
            .map_err(anyhow::Error::from)
            .context("failed to load WASM component")?;

        Self::from_component(engine, component, manifest, llm_provider, limits)
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
        let component = Component::from_binary(engine, bytes)
            .map_err(anyhow::Error::from)
            .context("failed to parse WASM component")?;

        Self::from_component(engine, component, manifest, llm_provider, limits)
    }

    fn from_component(
        engine: &Engine,
        component: Component,
        manifest: ModuleManifest,
        llm_provider: Arc<dyn LlmProvider>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        check_world(engine, &component)?;

        // Read what the plugin requests from an instance granted nothing,
        // then link what was granted (PL-U4).
        let mut module = Self {
            engine: engine.clone(),
            component,
            linker: build_linker(engine, &[Capability::Logging])?,
            manifest,
            llm_provider,
            limits,
            requested: Vec::new(),
            granted: vec![Capability::Logging],
            instance: None,
            last_metrics: None,
        };
        // Instantiate now so a module that can't instantiate fails to load.
        module.instance = Some(module.instantiate()?);
        let requested = module.read_requested()?;
        let granted = grants::resolve(&module.manifest.name, &module.manifest.grants, &requested)?;
        if granted != module.granted {
            module.linker = build_linker(engine, &granted)?;
            module.instance = Some(module.instantiate()?);
        }
        debug!(module = %module.manifest.name, ?requested, ?granted, "capabilities linked");
        module.requested = requested;
        module.granted = granted;
        Ok(module)
    }

    /// `metadata().requested-capabilities`, read at load. Bounded by the
    /// output ceiling rather than a lowered per-call cap: a cap meant for
    /// tool results must not stop the module from loading.
    fn read_requested(&mut self) -> Result<Vec<Capability>> {
        let cap = self.limits.max_output_bytes;
        self.limits.max_output_bytes = cap.max(MAX_OUTPUT_BYTES_CEILING);
        let metadata = self.metadata();
        self.limits.max_output_bytes = cap;
        Ok(metadata
            .context("failed to read the plugin's requested capabilities")?
            .requested_capabilities)
    }

    /// The capabilities the plugin's `metadata` requests.
    pub fn requested_capabilities(&self) -> &[Capability] {
        &self.requested
    }

    /// The capabilities the plugin is linked against: what was granted, and
    /// `logging`, in WIT order.
    pub fn granted_capabilities(&self) -> &[Capability] {
        &self.granted
    }

    /// Create a fresh store and instance of the component.
    fn instantiate(&self) -> Result<Instance> {
        let state = ModuleState::new(
            self.manifest.clone(),
            Arc::clone(&self.llm_provider),
            &self.limits,
        );
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limiter);
        // Instantiation may run guest code: bound it like a call.
        store
            .set_fuel(self.limits.max_fuel)
            .map_err(anyhow::Error::from)
            .context("failed to set fuel")?;
        store.set_epoch_deadline(epoch_ticks(self.limits.max_execution_ms));

        let bindings = PluginWorld::instantiate(&mut store, &self.component, &self.linker)
            .map_err(anyhow::Error::from)
            .context("failed to instantiate WASM module")?;
        debug!(module = %self.manifest.name, "WASM plugin instantiated ({})", crate::WIT_PACKAGE);
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
    // Guest export wrappers
    // -----------------------------------------------------------------------

    /// Call `plugin::invoke-tool` under the per-call limits.
    ///
    /// Returns the tool's result; the guest's own `tool-error` as a
    /// [`ToolFailure`] (reach it with `err.downcast_ref::<ToolFailure>()`); or a [`CallError`] (fuel,
    /// deadline, memory, trap, output size). Call
    /// [`Self::last_invocation_metrics`] after this for execution metrics.
    pub async fn invoke_tool(&mut self, call: ToolCallRequest) -> Result<ToolResult> {
        let result = self
            .call_blocking(
                "invoke-tool",
                move |bindings, store| {
                    bindings
                        .chatty_plugin_plugin()
                        .call_invoke_tool(store, &call)
                        .map(|result| result.map_err(ToolFailure::from))
                },
                tool_result_size,
            )
            .await;
        if let (Ok(result), Some(metrics)) = (&result, self.last_metrics.as_mut())
            && let Some(usage) = &result.usage
        {
            metrics.input_tokens = Some(usage.input_tokens);
            metrics.output_tokens = Some(usage.output_tokens);
        }
        result
    }

    /// Call `plugin::list-tools` (budget: [`METADATA_CALL_MS`]).
    pub fn list_tools(&mut self) -> Result<Vec<ToolDefinition>> {
        self.call_metadata(
            "list-tools",
            |bindings, store| {
                bindings
                    .chatty_plugin_plugin()
                    .call_list_tools(store)
                    .map(Ok)
            },
            |tools: &Result<Vec<ToolDefinition>, Infallible>| match tools {
                Ok(tools) => tools.iter().map(tool_definition_size).sum(),
                Err(never) => match *never {},
            },
        )
    }

    /// Call `plugin::metadata` (budget: [`METADATA_CALL_MS`]).
    pub fn metadata(&mut self) -> Result<PluginMetadata> {
        self.call_metadata(
            "metadata",
            |bindings, store| bindings.chatty_plugin_plugin().call_metadata(store).map(Ok),
            |metadata: &Result<PluginMetadata, Infallible>| match metadata {
                Ok(metadata) => metadata_size(metadata),
                Err(never) => match *never {},
            },
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

    /// Get the metrics from the most recent invocation.
    pub fn last_invocation_metrics(&self) -> Option<InvocationMetrics> {
        self.last_metrics.clone()
    }

    /// Run `call` off the async executor under the full per-call limits, and
    /// record [`InvocationMetrics`] for it.
    ///
    /// Where the call runs (AGE-707): on a multi-threaded Tokio runtime it
    /// runs in place, inside [`tokio::task::block_in_place`], which hands this
    /// worker's other tasks to another thread first and lets WASI's sync
    /// bindings `block_on`. That skips the thread hop per call PL-H1 paid
    /// with `spawn_blocking`. On a current-thread runtime `block_in_place` is
    /// not allowed, so the call still hops to `spawn_blocking`; with no Tokio
    /// runtime at all the caller's thread is not a Tokio executor and the call
    /// runs on it. Either way a panic in the call is caught and becomes
    /// [`CallError::HostPanic`], and the instance it was using is dropped.
    ///
    /// Running in place means the awaiting task itself cannot make progress
    /// (another branch of its `select!`, a timeout around this future) until
    /// the call returns; the call's own deadline bounds that wait.
    async fn call_blocking<O, E>(
        &mut self,
        export: &'static str,
        call: impl FnOnce(&PluginWorld, &mut Store<ModuleState>) -> wasmtime::Result<Result<O, E>>
        + Send
        + 'static,
        size: fn(&Result<O, E>) -> usize,
    ) -> Result<O>
    where
        O: Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let mut instance = self.take_instance()?;
        let limits = self.limits.clone();
        let start = Instant::now();

        let run = move || {
            let budget = limits.max_execution_ms;
            let report = run_export(&mut instance, &limits, budget, call, size);
            (instance, report)
        };
        let joined = match tokio::runtime::Handle::try_current() {
            Ok(runtime) if runtime.runtime_flavor() == RuntimeFlavor::CurrentThread => {
                tokio::task::spawn_blocking(run)
                    .await
                    .map_err(|join| join.to_string())
            }
            Ok(_) => catch_unwind(AssertUnwindSafe(|| tokio::task::block_in_place(run)))
                .map_err(|panic| panic_message(&*panic)),
            Err(_) => catch_unwind(AssertUnwindSafe(run)).map_err(|panic| panic_message(&*panic)),
        };

        let (result, fuel_consumed) = match joined {
            Ok((instance, report)) => {
                if !report.trapped {
                    self.instance = Some(instance);
                }
                (report.result, report.fuel_consumed)
            }
            Err(panic) => (Err(host_panic(panic)), 0),
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
    fn call_metadata<O, E>(
        &mut self,
        export: &'static str,
        call: impl FnOnce(&PluginWorld, &mut Store<ModuleState>) -> wasmtime::Result<Result<O, E>>
        + Send,
        size: fn(&Result<O, E>) -> usize,
    ) -> Result<O>
    where
        O: Send,
        E: std::error::Error + Send + Sync + 'static,
    {
        let mut instance = self.take_instance()?;
        let limits = &self.limits;
        let budget = METADATA_CALL_MS.min(limits.max_execution_ms);

        let joined = std::thread::scope(|scope| {
            scope
                .spawn(|| run_export(&mut instance, limits, budget, call, size))
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

/// A linker with WASI (its clock bounded by the call deadline) and the
/// plugin imports, the capabilities in `granted` real and the rest refused.
fn build_linker(engine: &Engine, granted: &[Capability]) -> Result<Linker<ModuleState>> {
    let mut linker: Linker<ModuleState> = Linker::new(engine);
    // WASI Preview 2 first — modules compiled for wasm32-wasip2 import WASI
    // interfaces (e.g. wasi:io/poll) from the host even when they don't
    // actively use them.
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(anyhow::Error::from)
        .context("failed to add WASI to linker")?;
    // ...with every clock wait capped at the call deadline (AGE-706).
    add_deadline_clock_to_linker(&mut linker)
        .map_err(anyhow::Error::from)
        .context("failed to add the deadline-bounded WASI clock to linker")?;
    grants::add_to_linker(&mut linker, granted).context("failed to add host imports to linker")?;
    Ok(linker)
}

/// Refuse a component that does not export `chatty:plugin/plugin@0.4.0`,
/// naming the world it does target (PL-D1: no older world is adapted).
fn check_world(engine: &Engine, component: &Component) -> Result<()> {
    let ty = component.component_type();
    let exports: Vec<String> = ty
        .exports(engine)
        .map(|(name, _)| name.to_string())
        .collect();
    if exports.iter().any(|name| name == crate::PLUGIN_EXPORT) {
        return Ok(());
    }
    let found = exports
        .iter()
        .find_map(|name| world_package(name))
        .unwrap_or_else(|| "no chatty world".to_string());
    Err(UnsupportedWorld { found }.into())
}

/// The package an export belongs to, `chatty:module@0.2.0` for
/// `chatty:module/agent@0.2.0`, when it is a chatty interface.
fn world_package(export: &str) -> Option<String> {
    let (package, rest) = export.split_once('/')?;
    if !package.starts_with("chatty:") {
        return None;
    }
    Some(match rest.split_once('@') {
        Some((_, version)) => format!("{package}@{version}"),
        None => package.to_string(),
    })
}

/// Flatten a call's outcome into the wrapper's `Result`: the guest's own
/// error keeps its type (downcast to it), a limit or trap stays a
/// [`CallError`].
fn finish<O, E>(export: &str, result: Result<Result<O, E>>) -> Result<O>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(guest)) => {
            Err(anyhow::Error::new(guest).context(format!("plugin::{export} returned an error")))
        }
        Err(e) => Err(e.context(format!("plugin::{export} failed"))),
    }
}

// ---------------------------------------------------------------------------
// One export call under the limits
// ---------------------------------------------------------------------------

/// What one export call produced.
struct CallReport<O, E> {
    /// The guest's own `Ok`/`Err`, or why the call failed.
    result: Result<Result<O, E>>,
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
fn run_export<O, E>(
    instance: &mut Instance,
    limits: &ResourceLimits,
    budget_ms: u64,
    call: impl FnOnce(&PluginWorld, &mut Store<ModuleState>) -> wasmtime::Result<Result<O, E>>,
    size: fn(&Result<O, E>) -> usize,
) -> CallReport<O, E> {
    let store = &mut instance.store;
    if let Err(e) = store.set_fuel(limits.max_fuel) {
        return CallReport {
            result: Err(anyhow::Error::from(e).context("failed to set fuel")),
            fuel_consumed: 0,
            trapped: false,
        };
    }
    store.set_epoch_deadline(epoch_ticks(budget_ms));
    {
        let state = store.data_mut();
        state.deadline = Some(Instant::now() + Duration::from_millis(budget_ms));
        state.deadline_hit = false;
        // A clock pollable the guest kept from an earlier call still holds
        // that call's flag; give this call its own so it can't be marked.
        match Arc::get_mut(&mut state.clock_deadline_hit) {
            Some(hit) => *hit.get_mut() = false,
            None => state.clock_deadline_hit = Arc::default(),
        }
        state.limiter.memory_denied = false;
        state.stderr.clear();
    }

    let outcome = call(&instance.bindings, store);

    let fuel_consumed = limits
        .max_fuel
        .saturating_sub(store.get_fuel().unwrap_or(0));
    let state = store.data_mut();
    state.deadline = None;
    let deadline_hit = state.deadline_hit || state.clock_deadline_hit.load(Ordering::Relaxed);
    let deadline = CallError::DeadlineExceeded {
        max_execution_ms: budget_ms,
    };

    let (result, trapped) = match outcome {
        Err(err) => (Err(classify_trap(&err, state, limits, budget_ms)), true),
        Ok(_) if deadline_hit => (Err(deadline), false),
        Ok(returned) => {
            let bytes = size(&returned);
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
        _ if err.downcast_ref::<NotGranted>().is_some() => CallError::NotGranted {
            capability: err
                .downcast_ref::<NotGranted>()
                .map_or("unknown", |refused| refused.0.name()),
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

fn tool_result_size(result: &Result<ToolResult, ToolFailure>) -> usize {
    match result {
        Ok(result) => result.content.len(),
        Err(error) => error.message.len(),
    }
}

fn tool_definition_size(tool: &ToolDefinition) -> usize {
    tool.name.len() + tool.description.len() + tool.parameters_schema.len()
}

fn metadata_size(metadata: &PluginMetadata) -> usize {
    metadata.name.len()
        + metadata.version.len()
        + metadata.description.len()
        + metadata
            .config_keys
            .iter()
            .map(|k| k.name.len() + k.description.len())
            .sum::<usize>()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::chatty::plugin::types::{CompletionResponse, Message};

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
