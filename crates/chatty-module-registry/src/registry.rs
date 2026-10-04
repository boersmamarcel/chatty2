//! Module discovery, loading, and lifecycle management.
//!
//! [`ModuleRegistry`] scans a root directory for module subdirectories and
//! loads each one as a [`WasmModule`]. A changed module is picked up by
//! [`ModuleRegistry::reload`] or a new scan; an agent that runs a module as a
//! plugin loads its own instance when it is built (PL-U2), so there is no
//! file-system watcher.
//!
//! # Directory layout
//!
//! ```text
//! .chatty/modules/
//! └── echo/
//!     ├── module.toml
//!     ├── echo.wasm
//!     └── .chatty-install.json   (only for modules chatty installed)
//! ```
//!
//! A module with an install record loads only while its `.wasm` still
//! matches the recorded hash (see [`crate::install_record`]); one without
//! loads as [`TrustLevel::Local`].
//!
//! Every subdirectory that contains a `module.toml` file is treated as a
//! module.  The registry uses the `[module].name` field from the manifest
//! (not the directory name) as the lookup key; a name may be registered from
//! one directory only.
//!
//! # Concurrency
//!
//! Each module sits behind its own [`ModuleHandle`] (an async mutex), so a
//! caller holds the registry only long enough to look a module up and clone
//! its handle, then calls the guest under that module's lock alone. A slow
//! call to one module never holds up a call to another (PL-H4, AGE-607).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use chatty_wasm_runtime::ModuleManifest as RuntimeManifest;
use chatty_wasm_runtime::{Capability, Engine, LlmProvider, ResourceLimits, WasmModule};

use crate::install_record::{ModuleGrants, verify_installed};
use crate::manifest::ModuleManifest;
use hive_client::TrustLevel;

// ---------------------------------------------------------------------------
// LoadedModule
// ---------------------------------------------------------------------------

/// A shared handle to one loaded module. Lock it for the length of one guest
/// call; calls to the same module queue on it, calls to different modules
/// do not contend.
pub type ModuleHandle = Arc<Mutex<WasmModule>>;

/// An entry in the registry: the parsed manifest plus the live module.
struct LoadedModule {
    manifest: ModuleManifest,
    /// The trust the module loaded at: its install record's, or
    /// [`TrustLevel::Local`] for one put there by hand (PL-H5a).
    trust_level: TrustLevel,
    /// Directory that the module was loaded from (needed for reload).
    module_dir: PathBuf,
    /// What its `metadata` requests (PL-U4), read at load.
    requested: Vec<Capability>,
    wasm: ModuleHandle,
}

// ---------------------------------------------------------------------------
// ScanReport
// ---------------------------------------------------------------------------

/// What [`ModuleRegistry::scan_directory`] did with each module directory.
///
/// Every list is in directory-name order.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Local modules now loaded into the registry: (module dir, manifest).
    /// A manifest's [`warnings`](ModuleManifest::warnings) (e.g. a clamped
    /// `[resources]` value) are reported here.
    pub loaded: Vec<(PathBuf, ModuleManifest)>,
    /// Remote modules: the manifest is valid, but they run on the
    /// hive-runner, so nothing was loaded into the WASM runtime.
    pub remote: Vec<(PathBuf, ModuleManifest)>,
    /// Directories that did not load, with the reason (an invalid manifest,
    /// a missing or broken `.wasm`, a duplicate module name, ...).
    pub failed: Vec<(PathBuf, String)>,
}

impl ScanReport {
    /// The names of the loaded local modules.
    pub fn loaded_names(&self) -> Vec<&str> {
        self.loaded.iter().map(|(_, m)| m.name.as_str()).collect()
    }

    /// The names of the remote modules.
    pub fn remote_names(&self) -> Vec<&str> {
        self.remote.iter().map(|(_, m)| m.name.as_str()).collect()
    }

    fn fail(&mut self, module_dir: PathBuf, error: anyhow::Error) {
        warn!(dir = %module_dir.display(), error = %format!("{error:#}"), "failed to load module — skipping");
        self.failed.push((module_dir, format!("{error:#}")));
    }
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
/// let report = registry.scan_directory(".chatty/modules")?;
/// for (dir, reason) in &report.failed {
///     eprintln!("{}: {reason}", dir.display());
/// }
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
    /// Directories are visited in name order. One that fails to load is
    /// reported in [`ScanReport::failed`] and skipped; it does **not** make
    /// this call return an error (only an unreadable `root_dir` does).
    ///
    /// Two directories declaring the same module name: the first in name
    /// order wins and the second is a failure. So is a name already loaded
    /// into this registry from a different directory.
    pub fn scan_directory(&mut self, root_dir: impl AsRef<Path>) -> Result<ScanReport> {
        let root_dir = root_dir.as_ref();
        info!(dir = %root_dir.display(), "scanning for WASM modules");

        let entries = std::fs::read_dir(root_dir)
            .with_context(|| format!("failed to read module directory {}", root_dir.display()))?;

        let mut module_dirs = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let module_dir = entry.path();
                    if !module_dir.is_dir() {
                        continue;
                    }
                    if !module_dir.join("module.toml").exists() {
                        debug!(dir = %module_dir.display(), "skipping: no module.toml");
                        continue;
                    }
                    module_dirs.push(module_dir);
                }
                Err(e) => warn!(error = %e, "error reading directory entry"),
            }
        }
        module_dirs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        let mut report = ScanReport::default();
        // Module name → the directory that declared it first in this scan.
        let mut claimed: HashMap<String, PathBuf> = HashMap::new();

        for module_dir in module_dirs {
            let manifest = match ModuleManifest::from_file(&module_dir.join("module.toml")) {
                Ok(manifest) => manifest,
                Err(e) => {
                    report.fail(module_dir, e);
                    continue;
                }
            };
            if let Some(first) = claimed.get(&manifest.name) {
                let reason = anyhow::anyhow!(
                    "duplicate module name '{}': already declared by {}",
                    manifest.name,
                    first.display()
                );
                report.fail(module_dir, reason);
                continue;
            }
            claimed.insert(manifest.name.clone(), module_dir.clone());
            for warning in &manifest.warnings {
                warn!(module = %manifest.name, dir = %module_dir.display(), %warning, "module manifest");
            }

            // Remote modules have no local WASM binary — they execute on the
            // hive-runner, and the gateway routes them there.
            if manifest.execution_mode.is_remote() {
                debug!(module = %manifest.name, "remote module: not loaded into the WASM runtime");
                report.remote.push((module_dir, manifest));
                continue;
            }

            match self.insert_local(&module_dir, manifest.clone()) {
                Ok(()) => {
                    info!(module = %manifest.name, dir = %module_dir.display(), "loaded module");
                    report.loaded.push((module_dir, manifest));
                }
                Err(e) => report.fail(module_dir, e),
            }
        }

        Ok(report)
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

    /// The [`ModuleHandle`] of the module with the given name, or `None` if
    /// it is not registered.
    ///
    /// The handle outlives the registry borrow: clone it out, release the
    /// registry, then lock the module for the call. A module unloaded or
    /// reloaded meanwhile finishes the call on the instance it started with.
    pub fn get(&self, name: &str) -> Option<ModuleHandle> {
        self.modules.get(name).map(|m| Arc::clone(&m.wasm))
    }

    /// Return the parsed [`ModuleManifest`] for a registered module.
    pub fn manifest(&self, name: &str) -> Option<&ModuleManifest> {
        self.modules.get(name).map(|m| &m.manifest)
    }

    /// The trust a registered module loaded at: its install record's level,
    /// or [`TrustLevel::Local`] for a module with no record (copied in by
    /// hand). `None` if it is not registered.
    pub fn trust_level(&self, name: &str) -> Option<TrustLevel> {
        self.modules.get(name).map(|m| m.trust_level.clone())
    }

    /// The capabilities a registered module's `metadata` requests (PL-U4),
    /// in its order. `None` if it is not registered.
    pub fn requested_capabilities(&self, name: &str) -> Option<&[Capability]> {
        self.modules.get(name).map(|m| m.requested.as_slice())
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
    // Internal helpers
    // -----------------------------------------------------------------------

    fn load_from_dir(&mut self, module_dir: &Path) -> Result<String> {
        let manifest = ModuleManifest::from_file(&module_dir.join("module.toml"))?;
        let name = manifest.name.clone();

        // Remote modules have no local WASM binary — they execute on the
        // hive-runner.  Skip them; the gateway routes them via the runner's
        // OpenAI-compat endpoint instead.
        if manifest.execution_mode.is_remote() {
            debug!(module = %name, "remote module: not loaded into the WASM runtime");
            return Ok(name);
        }

        self.insert_local(module_dir, manifest)?;
        Ok(name)
    }

    /// Load a local module's `.wasm` and register it under its name.
    ///
    /// Refuses a name already registered from a different directory; the
    /// same directory is a reload and replaces the old instance.
    fn insert_local(&mut self, module_dir: &Path, manifest: ModuleManifest) -> Result<()> {
        if let Some(existing) = self.modules.get(&manifest.name)
            && existing.module_dir != module_dir
        {
            anyhow::bail!(
                "duplicate module name '{}': already loaded from {}",
                manifest.name,
                existing.module_dir.display()
            );
        }

        let wasm_path = manifest.wasm_path.as_ref().with_context(|| {
            format!(
                "manifest in {} declares local execution but has no wasm path",
                module_dir.display()
            )
        })?;

        // Build resource limits from manifest, falling back to defaults.
        let limits = self.limits_from_manifest(&manifest);

        // The runtime's manifest carries what the guest sees: `[config]`
        // through `config::get`, `[files].root` through `file::read-bytes`.
        // A module served here has no agent spec to grant from: installing
        // it gets `config` by default; `llm` and `file` link only when the
        // user granted them in Settings (SEC-11, AGE-815).
        let approved = ModuleGrants::read(module_dir)
            .with_context(|| format!("failed to read grants of '{}'", manifest.name))?
            .capabilities();
        let mut runtime_manifest = manifest
            .config
            .iter()
            .fold(RuntimeManifest::new(&manifest.name), |m, (key, value)| {
                m.with_config(key, value)
            })
            .with_specless_grants(approved);
        if let Some(root) = &manifest.files_root {
            runtime_manifest = runtime_manifest.with_weights_root(root);
        }

        let load_context = || {
            format!(
                "failed to load WASM module '{}' from {}",
                manifest.name,
                wasm_path.display()
            )
        };
        // Read the bytes once: the hash is checked against the install
        // record (PL-H5a) on exactly the bytes that are then compiled.
        let bytes = std::fs::read(wasm_path).with_context(load_context)?;
        let trust_level = verify_installed(module_dir, &bytes).with_context(load_context)?;
        let wasm = WasmModule::from_bytes(
            &self.engine,
            &bytes,
            runtime_manifest,
            self.llm_provider.clone(),
            limits,
        )
        .with_context(load_context)?;

        self.modules.insert(
            manifest.name.clone(),
            LoadedModule {
                manifest,
                trust_level,
                module_dir: module_dir.to_path_buf(),
                requested: wasm.requested_capabilities().to_vec(),
                wasm: Arc::new(Mutex::new(wasm)),
            },
        );

        Ok(())
    }

    /// The registry's limits, lowered by the manifest's `[resources]`.
    ///
    /// A manifest may only lower a limit (0 means "not set"), and the result
    /// never exceeds the host ceilings (PL-D3; the manifest's values are
    /// already clamped to them at parse time).
    fn limits_from_manifest(&self, manifest: &ModuleManifest) -> ResourceLimits {
        let mut limits = self.default_limits.clone();

        if manifest.resources.max_memory_mb > 0 {
            let bytes = manifest.resources.max_memory_mb.saturating_mul(1024 * 1024);
            limits.max_memory_bytes = limits.max_memory_bytes.min(bytes);
        }

        if manifest.resources.max_execution_ms > 0 {
            limits.max_execution_ms = limits
                .max_execution_ms
                .min(manifest.resources.max_execution_ms);
        }

        limits.clamped()
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
        let report = reg.scan_directory(tmp.path()).unwrap();
        assert!(report.loaded.is_empty() && report.remote.is_empty() && report.failed.is_empty());
    }

    #[test]
    fn scan_skips_directory_without_module_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("not-a-module")).unwrap();
        let mut reg = noop_registry();
        let report = reg.scan_directory(tmp.path()).unwrap();
        assert!(report.loaded.is_empty() && report.remote.is_empty() && report.failed.is_empty());
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
    fn manifest_limits_are_clamped_to_ceilings() {
        let reg = noop_registry();
        let manifest = crate::manifest::ModuleManifest::from_str(
            r#"
[module]
name = "x"
version = "1.0.0"
wasm = "x.wasm"

[resources]
max_memory_mb = 9007199254740992
max_execution_ms = 9223372036854775807
"#,
            std::path::Path::new("/fake/module.toml"),
        )
        .unwrap();

        let limits = reg.limits_from_manifest(&manifest);
        assert_eq!(
            limits.max_memory_bytes,
            chatty_wasm_runtime::MAX_MEMORY_BYTES_CEILING
        );
        assert_eq!(
            limits.max_execution_ms,
            chatty_wasm_runtime::MAX_EXECUTION_MS_CEILING
        );
        assert_eq!(limits.max_fuel, chatty_wasm_runtime::MAX_FUEL_CEILING);
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
