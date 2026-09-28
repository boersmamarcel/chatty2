use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, trace, warn};
use wasmtime::component::{Linker, Resource};
use wasmtime::{ResourceLimiter, StoreLimits};
use wasmtime_wasi::bindings::clocks::monotonic_clock;
use wasmtime_wasi::{
    DynPollable, IoImpl, IoView, OutputStream, Pollable, ResourceTable, StdoutStream, StreamError,
    WasiCtx, WasiCtxBuilder, WasiImpl, WasiView,
};

use crate::bindings::chatty::plugin::billing::SessionInfo;
use crate::bindings::chatty::plugin::types::{CompletionResponse, Message};
use crate::limits::{MAX_FILE_READ_BYTES, ResourceLimits};

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
/// use chatty_wasm_runtime::SessionInfo;
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
/// the guest when it calls the `config::get` host import, and its file root
/// is the only directory `file::read-bytes` may read from.
#[derive(Debug, Clone, Default)]
pub struct ModuleManifest {
    /// Human-readable module name used as a prefix in log messages.
    pub name: String,
    /// Arbitrary key-value configuration the module declared.
    config: std::collections::HashMap<String, String>,
    /// The directory `file::read-bytes` resolves paths against. `None`
    /// means the module was granted no files and every read fails.
    weights_root: Option<PathBuf>,
}

impl ModuleManifest {
    /// Create a new manifest with the given name, no config entries and no
    /// file root.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            config: Default::default(),
            weights_root: None,
        }
    }

    /// Add or overwrite a configuration key-value pair.
    pub fn with_config(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.insert(key.into(), value.into());
        self
    }

    /// Grant the guest read access to the files under `root` (and nothing
    /// else) through `file::read-bytes`. A config key named `weights_root`
    /// grants nothing: the root is host-set, never guest-visible config.
    pub fn with_weights_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.weights_root = Some(root.into());
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
    /// Set when a WASI clock wait was cut short at `deadline` (AGE-706).
    /// Shared with the pollable, which fires on WASI's own executor.
    pub(crate) clock_deadline_hit: Arc<AtomicBool>,
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
            clock_deadline_hit: Arc::default(),
            stderr,
            manifest,
            llm_provider,
            billing_provider,
            wasi_ctx,
            table,
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

// ---------------------------------------------------------------------------
// Deadline-bounded WASI monotonic clock (AGE-706)
// ---------------------------------------------------------------------------

/// Replace `wasi:clocks/monotonic-clock` in `linker` (after
/// `wasmtime_wasi::add_to_linker_sync` defined it) with [`DeadlineClock`].
///
/// A guest that sleeps (`std::thread::sleep`, i.e. `subscribe-duration` +
/// `wasi:io/poll`) blocks inside WASI's own `block_on`, where no epoch check
/// runs, so the epoch deadline alone can't end the call. The only waits a
/// guest can start are clock subscriptions (the `WasiCtx` grants no
/// sockets, files or stdin), so bounding those bounds every WASI wait.
pub(crate) fn add_deadline_clock_to_linker(linker: &mut Linker<ModuleState>) -> anyhow::Result<()> {
    linker.allow_shadowing(true);
    let added = monotonic_clock::add_to_linker_get_host(linker, deadline_clock);
    linker.allow_shadowing(false);
    added
}

fn deadline_clock(state: &mut ModuleState) -> DeadlineClock<'_> {
    DeadlineClock(state)
}

/// `wasi:clocks/monotonic-clock` with every subscription capped at the
/// current call's deadline. `now` and `resolution`, and any wait that ends
/// before the deadline, are WASI's own.
struct DeadlineClock<'a>(&'a mut ModuleState);

impl DeadlineClock<'_> {
    fn wasi(&mut self) -> WasiImpl<&mut ModuleState> {
        WasiImpl(IoImpl(&mut *self.0))
    }

    /// A pollable for a `wait`, or `None` when it ends by the deadline
    /// (WASI's own pollable is used then).
    ///
    /// A wait past the deadline becomes one that fires *at* the deadline and
    /// sets `clock_deadline_hit`, so the call fails with `deadline exceeded`
    /// (and a guest still running afterwards is stopped by the epoch).
    /// Longer waits are only cut short, not refused: a guest may subscribe a
    /// long timeout and poll it beside a shorter one.
    fn capped(&mut self, wait: Duration) -> anyhow::Result<Option<Resource<DynPollable>>> {
        let Some(deadline) = self.0.deadline else {
            return Ok(None);
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if wait <= remaining {
            return Ok(None);
        }
        let cut = CutAtDeadline {
            at: tokio::time::Instant::now() + remaining,
            hit: Arc::clone(&self.0.clock_deadline_hit),
        };
        let resource = self.0.table.push(cut)?;
        Ok(Some(wasmtime_wasi::subscribe(&mut self.0.table, resource)?))
    }
}

impl monotonic_clock::Host for DeadlineClock<'_> {
    fn now(&mut self) -> anyhow::Result<monotonic_clock::Instant> {
        monotonic_clock::Host::now(&mut self.wasi())
    }

    fn resolution(&mut self) -> anyhow::Result<monotonic_clock::Duration> {
        monotonic_clock::Host::resolution(&mut self.wasi())
    }

    fn subscribe_instant(
        &mut self,
        when: monotonic_clock::Instant,
    ) -> anyhow::Result<Resource<DynPollable>> {
        let now = monotonic_clock::Host::now(&mut self.wasi())?;
        match self.capped(Duration::from_nanos(when.saturating_sub(now)))? {
            Some(pollable) => Ok(pollable),
            None => monotonic_clock::Host::subscribe_instant(&mut self.wasi(), when),
        }
    }

    fn subscribe_duration(
        &mut self,
        duration: monotonic_clock::Duration,
    ) -> anyhow::Result<Resource<DynPollable>> {
        match self.capped(Duration::from_nanos(duration))? {
            Some(pollable) => Ok(pollable),
            None => monotonic_clock::Host::subscribe_duration(&mut self.wasi(), duration),
        }
    }
}

/// A clock wait cut short at the call deadline: ready at `at`, and marks
/// `hit` when it fires.
struct CutAtDeadline {
    at: tokio::time::Instant,
    hit: Arc<AtomicBool>,
}

#[wasmtime_wasi::async_trait]
impl Pollable for CutAtDeadline {
    async fn ready(&mut self) {
        tokio::time::sleep_until(self.at).await;
        self.hit.store(true, Ordering::Relaxed);
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
impl crate::bindings::chatty::plugin::types::Host for ModuleState {}

impl crate::bindings::chatty::plugin::llm::Host for ModuleState {
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

impl crate::bindings::chatty::plugin::config::Host for ModuleState {
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

impl crate::bindings::chatty::plugin::logging::Host for ModuleState {
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

impl crate::bindings::chatty::plugin::billing::Host for ModuleState {
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

impl crate::bindings::chatty::plugin::file::Host for ModuleState {
    fn read_bytes(&mut self, path: String) -> Result<Vec<u8>, String> {
        let root = self.manifest.weights_root.clone().ok_or_else(|| {
            "file::read_bytes: this module was granted no file root ([files] root)".to_string()
        })?;
        let relative = sandboxed_relative_path(&path)?;

        debug!(
            module = %self.manifest.name,
            root = %root.display(),
            path = %relative.display(),
            "file::read_bytes"
        );

        let result = self.before_deadline("file-read-bytes", move || {
            read_inside_root(&root, &relative, MAX_FILE_READ_BYTES)
        });
        if let Err(ref e) = result {
            warn!(module = %self.manifest.name, path = %path, error = %e, "file::read_bytes failed");
        }
        result
    }
}

/// A guest-supplied `file::read-bytes` path as a plain relative path.
///
/// `/` and `\` both separate components (so a Windows-style path means the
/// same thing on every host); empty and `.` components are dropped. Rejected:
/// an empty path, a leading separator (absolute), a `:` anywhere (a Windows
/// drive or stream), and any `..` component.
fn sandboxed_relative_path(path: &str) -> Result<PathBuf, String> {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') || Path::new(path).has_root() {
        return Err(format!("file::read_bytes: absolute path rejected: {path}"));
    }
    if normalized.contains(':') {
        return Err(format!(
            "file::read_bytes: drive or stream path rejected: {path}"
        ));
    }
    let mut relative = PathBuf::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(format!("file::read_bytes: `..` component rejected: {path}"));
            }
            name => relative.push(name),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(format!("file::read_bytes: empty path rejected: {path:?}"));
    }
    Ok(relative)
}

/// Read `root/relative`, refusing anything that resolves outside `root`.
///
/// Both sides are canonicalized, so a symlink (at any component) that leads
/// out of the root is refused while one that stays inside is followed. Only
/// regular files are read, and a file over `cap` bytes is refused from its
/// metadata before any of it is read. Error text never names host paths:
/// it goes back to the guest.
fn read_inside_root(root: &Path, relative: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("file::read_bytes: the file root is unavailable: {e}"))?;
    let resolved = root
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("file::read_bytes: {e}"))?;
    if !resolved.starts_with(&root) {
        return Err("file::read_bytes: path resolves outside the file root".to_string());
    }
    let file = std::fs::File::open(&resolved).map_err(|e| format!("file::read_bytes: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("file::read_bytes: {e}"))?;
    if !metadata.is_file() {
        return Err("file::read_bytes: not a regular file".to_string());
    }
    let too_large =
        |bytes: u64| format!("file::read_bytes: file is {bytes} bytes, over the {cap}-byte cap");
    if metadata.len() > cap {
        return Err(too_large(metadata.len()));
    }
    // The file may grow between the metadata check and the read.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("file::read_bytes: {e}"))?;
    if bytes.len() as u64 > cap {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
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
        use crate::bindings::chatty::plugin::config::Host;
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
        use crate::bindings::chatty::plugin::llm::Host;
        let provider: Arc<dyn LlmProvider> = Arc::new(EchoProvider {
            response: "!".to_string(),
        });
        let mut state = make_state(provider);
        let messages = vec![Message {
            role: crate::bindings::chatty::plugin::types::Role::User,
            content: "hello".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        }];
        let result = state.complete("gpt-4".to_string(), messages, None);
        let resp = result.unwrap();
        assert_eq!(resp.content, "echo: hello!");
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn llm_host_propagates_provider_error() {
        use crate::bindings::chatty::plugin::llm::Host;
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
        use crate::bindings::chatty::plugin::llm::Host;
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
        use crate::bindings::chatty::plugin::llm::Host;
        let mut state = make_state(Arc::new(PanickingProvider));
        state.deadline = Some(Instant::now() + std::time::Duration::from_secs(5));
        let err = state.complete("m".to_string(), vec![], None).unwrap_err();
        assert!(err.contains("panicked"), "{err}");
        assert!(!state.deadline_hit);
    }

    #[test]
    fn logging_host_does_not_panic() {
        use crate::bindings::chatty::plugin::logging::Host;
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

    #[test]
    fn guest_paths_are_plain_relative_paths() {
        let ok = |p: &str| sandboxed_relative_path(p).expect(p);
        assert_eq!(ok("a.bin"), PathBuf::from("a.bin"));
        assert_eq!(ok("sub/a.bin"), Path::new("sub").join("a.bin"));
        // Windows separators mean the same thing on every host.
        assert_eq!(ok("sub\\a.bin"), Path::new("sub").join("a.bin"));
        assert_eq!(ok("./sub//a.bin"), Path::new("sub").join("a.bin"));

        for bad in [
            "",
            ".",
            "/etc/passwd",
            "\\etc\\passwd",
            "C:\\Windows\\win.ini",
            "C:a.bin",
            "a.bin:stream",
            "..",
            "../secret",
            "sub/../../secret",
            "sub\\..\\..\\secret",
        ] {
            assert!(
                sandboxed_relative_path(bad).is_err(),
                "`{bad}` must be rejected"
            );
        }
    }

    #[test]
    fn reads_are_capped_before_reading() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("small"), b"12345").unwrap();
        assert_eq!(
            read_inside_root(root.path(), Path::new("small"), 5).unwrap(),
            b"12345"
        );
        let err = read_inside_root(root.path(), Path::new("small"), 4).unwrap_err();
        assert!(err.contains("over the 4-byte cap"), "{err}");
    }

    #[test]
    fn directories_are_not_read() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let err = read_inside_root(root.path(), Path::new("sub"), 1024).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_followed_only_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("real"), b"in").unwrap();
        std::fs::write(outside.path().join("secret"), b"out").unwrap();
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("inner")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape-dir")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("escape"))
            .unwrap();

        assert_eq!(
            read_inside_root(root.path(), Path::new("inner"), 1024).unwrap(),
            b"in"
        );
        for escape in ["escape", "escape-dir/secret"] {
            let err = read_inside_root(root.path(), Path::new(escape), 1024).unwrap_err();
            assert!(err.contains("outside the file root"), "{escape}: {err}");
            assert!(
                !err.contains(&*outside.path().to_string_lossy()),
                "a guest-visible error must not name host paths: {err}"
            );
        }
    }

    #[test]
    fn a_weights_root_config_key_grants_no_files() {
        use crate::bindings::chatty::plugin::file::Host;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.bin"), b"x").unwrap();
        let provider: Arc<dyn LlmProvider> = Arc::new(ErrorProvider);
        let manifest = ModuleManifest::new("m")
            .with_config("weights_root", root.path().to_string_lossy().to_string());
        let mut state = ModuleState::new(manifest, provider, None, &ResourceLimits::default());
        let err = state.read_bytes("a.bin".to_string()).unwrap_err();
        assert!(err.contains("no file root"), "{err}");
    }
}
