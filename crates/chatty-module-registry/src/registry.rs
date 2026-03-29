//! Module discovery, loading, and lifecycle management.
//!
//! [`ModuleRegistry`] scans a root directory for module subdirectories, loads
//! each one as a [`WasmModule`], and provides hot-reload via the
//! [`notify`] file-system watcher.
//!
//! # Directory layout
//!
//! ```text
//! .chatty/modules/
//! └── echo-agent/
//!     ├── module.toml
//!     └── echo_agent.wasm
//! ```
//!
//! Every subdirectory that contains a `module.toml` file is treated as a
//! module.  The registry uses the `[module].name` field from the manifest
//! (not the directory name) as the lookup key.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use chatty_wasm_runtime::ModuleManifest as RuntimeManifest;
use chatty_wasm_runtime::{Engine, LlmProvider, ResourceLimits, WasmModule};

use crate::manifest::ModuleManifest;

// ---------------------------------------------------------------------------
// LoadedModule
// ---------------------------------------------------------------------------

/// An entry in the registry: the parsed manifest plus the live module.
struct LoadedModule {
    manifest: ModuleManifest,
    /// Directory that the module was loaded from (needed for reload).
    module_dir: PathBuf,
    wasm: WasmModule,
}

// ---------------------------------------------------------------------------
// ModuleRegistry
// ---------------------------------------------------------------------------

/// Registry that discovers, loads, and manages the lifecycle of WASM modules.
///
/// # Usage
///
/// ```rust,no_run
/// # use std::sync::Arc;
/// # use chatty_module_registry::ModuleRegistry;
/// # use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
/// # use chatty_wasm_runtime::ModuleManifest as RuntimeManifest;
/// # struct NoopProvider;
/// # impl LlmProvider for NoopProvider {
/// #     fn complete(&self, _: &str, _: Vec<chatty_wasm_runtime::Message>, _: Option<String>)
/// #         -> Result<chatty_wasm_runtime::CompletionResponse, String> { Err("noop".into()) }
/// # }
/// # async fn run() -> anyhow::Result<()> {
/// let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
/// let mut registry = ModuleRegistry::new(provider, ResourceLimits::default())?;
/// registry.scan_directory(".chatty/modules")?;
/// # Ok(())
/// # }
/// ```
pub struct ModuleRegistry {
    engine: Engine,
    modules: HashMap<String, LoadedModule>,
    llm_provider: Arc<dyn LlmProvider>,
    default_limits: ResourceLimits,
}

impl ModuleRegistry {
    /// Create a new, empty registry.
    ///
    /// A shared [`Engine`] is built once and reused for all modules loaded
    /// into this registry.
    pub fn new(llm_provider: Arc<dyn LlmProvider>, default_limits: ResourceLimits) -> Result<Self> {
        let engine =
            WasmModule::build_engine(&default_limits).context("failed to build Wasmtime engine")?;

        Ok(Self {
            engine,
            modules: HashMap::new(),
            llm_provider,
            default_limits,
        })
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    /// Scan `root_dir` for module sub-directories and load each one.
    ///
    /// A sub-directory is a module if it contains a `module.toml` file.
    /// Directories that fail to load are logged as warnings and skipped;
    /// they do **not** cause this call to return an error.
    ///
    /// Returns the names of all successfully loaded modules.
    pub fn scan_directory(&mut self, root_dir: impl AsRef<Path>) -> Result<Vec<String>> {
        let root_dir = root_dir.as_ref();
        info!(dir = %root_dir.display(), "scanning for WASM modules");

        let entries = std::fs::read_dir(root_dir)
            .with_context(|| format!("failed to read module directory {}", root_dir.display()))?;

        let mut loaded = Vec::new();

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "error reading directory entry");
                    continue;
                }
            };

            let module_dir = entry.path();
            if !module_dir.is_dir() {
                continue;
            }

            let manifest_path = module_dir.join("module.toml");
            if !manifest_path.exists() {
                debug!(dir = %module_dir.display(), "skipping: no module.toml");
                continue;
            }

            match self.load_from_dir(&module_dir) {
                Ok(name) => {
                    info!(module = %name, dir = %module_dir.display(), "loaded module");
                    loaded.push(name);
                }
                Err(e) => {
                    warn!(
                        dir = %module_dir.display(),
                        error = %e,
                        "failed to load module — skipping"
                    );
                }
            }
        }

        Ok(loaded)
    }

    // -----------------------------------------------------------------------
    // Load / unload / reload
    // -----------------------------------------------------------------------

    /// Load a single module from `module_dir`.
    ///
    /// `module_dir` must contain a `module.toml` and the `.wasm` file
    /// referenced by it.  Returns the module name on success.
    pub fn load(&mut self, module_dir: impl AsRef<Path>) -> Result<String> {
        let module_dir = module_dir.as_ref().to_path_buf();
        self.load_from_dir(&module_dir)
    }

    /// Unload a module by name, freeing its Wasmtime store.
    ///
    /// Returns an error if the module is not registered.
    pub fn unload(&mut self, name: &str) -> Result<()> {
        if self.modules.remove(name).is_some() {
            info!(module = %name, "unloaded module");
            Ok(())
        } else {
            anyhow::bail!("module '{}' is not registered", name)
        }
    }

    /// Hot-reload a module by name.
    ///
    /// The existing instance is dropped and a fresh one is loaded from the
    /// same directory.  Returns an error if the module is not registered or
    /// the reload fails.
    pub fn reload(&mut self, name: &str) -> Result<()> {
        let module_dir = self
            .modules
            .get(name)
            .map(|m| m.module_dir.clone())
            .with_context(|| format!("module '{}' is not registered", name))?;

        self.modules.remove(name);

        match self.load_from_dir(&module_dir) {
            Ok(new_name) => {
                info!(
                    module = %new_name,
                    dir = %module_dir.display(),
                    "hot-reloaded module"
                );
                Ok(())
            }
            Err(e) => {
                // Leave the slot empty rather than reverting — callers can
                // retry or re-scan.
                Err(e.context(format!("failed to reload module '{}'", name)))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Return an immutable reference to the loaded [`WasmModule`] with the
    /// given name, or `None` if it is not registered.
    pub fn get(&self, name: &str) -> Option<&WasmModule> {
        self.modules.get(name).map(|m| &m.wasm)
    }

    /// Return a mutable reference to the loaded [`WasmModule`] with the
    /// given name, or `None` if it is not registered.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut WasmModule> {
        self.modules.get_mut(name).map(|m| &mut m.wasm)
    }

    /// Return the parsed [`ModuleManifest`] for a registered module.
    pub fn manifest(&self, name: &str) -> Option<&ModuleManifest> {
        self.modules.get(name).map(|m| &m.manifest)
    }

    /// Return an iterator over the names of all registered modules.
    pub fn module_names(&self) -> impl Iterator<Item = &str> {
        self.modules.keys().map(String::as_str)
    }

    /// Return the number of currently loaded modules.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Return `true` if no modules are loaded.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    // -----------------------------------------------------------------------
    // File-system watching (hot-reload)
    // -----------------------------------------------------------------------

    /// Start a [`notify`] file-system watcher on `watch_dir`.
    ///
    /// Events are forwarded over an [`mpsc`] channel.  The caller is
    /// responsible for receiving events and calling [`Self::reload`] /
    /// [`Self::scan_directory`] as appropriate.
    ///
    /// The returned [`RecommendedWatcher`] must be kept alive; dropping it
    /// stops the watcher.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use chatty_module_registry::ModuleRegistry;
    /// # use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
    /// # use chatty_wasm_runtime::ModuleManifest as RuntimeManifest;
    /// # struct NoopProvider;
    /// # impl LlmProvider for NoopProvider {
    /// #     fn complete(&self, _: &str, _: Vec<chatty_wasm_runtime::Message>, _: Option<String>)
    /// #         -> Result<chatty_wasm_runtime::CompletionResponse, String> { Err("noop".into()) }
    /// # }
    /// # async fn run() -> anyhow::Result<()> {
    /// let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    /// let mut registry = ModuleRegistry::new(provider, ResourceLimits::default())?;
    /// let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    /// let _watcher = registry.watch(".chatty/modules", tx)?;
    /// while let Some(event) = rx.recv().await {
    ///     println!("fs event: {:?}", event);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn watch(
        &self,
        watch_dir: impl AsRef<Path>,
        sender: mpsc::Sender<notify::Result<Event>>,
    ) -> Result<RecommendedWatcher> {
        let watch_dir = watch_dir.as_ref().to_path_buf();

        let mut watcher = notify::recommended_watcher(move |res| {
            // Best-effort send; log a warning if the channel is full or closed
            // so that missed hot-reload events are visible in diagnostics.
            if sender.try_send(res).is_err() {
                warn!("fs watcher event dropped — channel full or closed");
            }
        })
        .context("failed to create file-system watcher")?;

        watcher
            .watch(&watch_dir, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", watch_dir.display()))?;

        info!(dir = %watch_dir.display(), "watching for module changes");
        Ok(watcher)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn load_from_dir(&mut self, module_dir: &Path) -> Result<String> {
        let manifest_path = module_dir.join("module.toml");

        let manifest = ModuleManifest::from_file(&manifest_path)?;

        // Build resource limits from manifest, falling back to defaults.
        let limits = self.limits_from_manifest(&manifest);

        // Build a RuntimeManifest (the chatty-wasm-runtime type) from our
        // parsed manifest so we can pass it to WasmModule::from_file.
        let runtime_manifest = RuntimeManifest::new(&manifest.name);

        let wasm = WasmModule::from_file(
            &self.engine,
            &manifest.wasm_path,
            runtime_manifest,
            self.llm_provider.clone(),
            limits,
        )
        .with_context(|| {
            format!(
                "failed to load WASM module '{}' from {}",
                manifest.name,
                manifest.wasm_path.display()
            )
        })?;

        let name = manifest.name.clone();

        self.modules.insert(
            name.clone(),
            LoadedModule {
                manifest,
                module_dir: module_dir.to_path_buf(),
                wasm,
            },
        );

        Ok(name)
    }

    fn limits_from_manifest(&self, manifest: &ModuleManifest) -> ResourceLimits {
        let mut limits = self.default_limits.clone();

        if manifest.resources.max_memory_mb > 0 {
            limits.max_memory_bytes = manifest.resources.max_memory_mb * 1024 * 1024;
        }

        if manifest.resources.max_execution_ms > 0 {
            limits.max_execution_ms = manifest.resources.max_execution_ms;
        }

        limits
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopProvider;

    impl LlmProvider for NoopProvider {
        fn complete(
            &self,
            _model: &str,
            _messages: Vec<chatty_wasm_runtime::Message>,
            _tools: Option<String>,
        ) -> Result<chatty_wasm_runtime::CompletionResponse, String> {
            Err("noop provider".into())
        }
    }

    fn noop_registry() -> ModuleRegistry {
        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap()
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = noop_registry();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn get_returns_none_for_unknown_module() {
        let reg = noop_registry();
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn unload_unknown_returns_error() {
        let mut reg = noop_registry();
        assert!(reg.unload("not-loaded").is_err());
    }

    #[test]
    fn reload_unknown_returns_error() {
        let mut reg = noop_registry();
        assert!(reg.reload("not-loaded").is_err());
    }

    #[test]
    fn scan_nonexistent_directory_returns_error() {
        let mut reg = noop_registry();
        assert!(reg.scan_directory("/nonexistent/modules").is_err());
    }

    #[test]
    fn scan_empty_directory_returns_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = noop_registry();
        let names = reg.scan_directory(tmp.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn scan_skips_directory_without_module_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("not-a-module")).unwrap();
        let mut reg = noop_registry();
        let names = reg.scan_directory(tmp.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn limits_from_manifest_uses_manifest_values() {
        let reg = noop_registry();
        let manifest = crate::manifest::ModuleManifest::from_str(
            r#"
[module]
name = "x"
version = "1.0.0"
wasm = "x.wasm"

[resources]
max_memory_mb = 128
max_execution_ms = 10000
"#,
            std::path::Path::new("/fake/module.toml"),
        )
        .unwrap();

        let limits = reg.limits_from_manifest(&manifest);
        assert_eq!(limits.max_memory_bytes, 128 * 1024 * 1024);
        assert_eq!(limits.max_execution_ms, 10000);
    }

    #[test]
    fn limits_from_manifest_falls_back_to_defaults_when_zero() {
        let reg = noop_registry();
        let manifest = crate::manifest::ModuleManifest::from_str(
            r#"
[module]
name = "x"
version = "1.0.0"
wasm = "x.wasm"
"#,
            std::path::Path::new("/fake/module.toml"),
        )
        .unwrap();

        let limits = reg.limits_from_manifest(&manifest);
        let defaults = ResourceLimits::default();
        assert_eq!(limits.max_memory_bytes, defaults.max_memory_bytes);
        assert_eq!(limits.max_execution_ms, defaults.max_execution_ms);
    }
}
