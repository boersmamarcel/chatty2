//! `chatty-module-registry` — module discovery, loading, and lifecycle management.
//!
//! This crate discovers `.wasm` modules from the filesystem, parses their
//! `module.toml` manifests, loads them via `chatty-wasm-runtime`, and
//! manages their lifecycle including hot-reload.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use chatty_module_registry::ModuleRegistry;
//! use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
//!
//! # struct NoopProvider;
//! # impl LlmProvider for NoopProvider {
//! #     fn complete(&self, _: &str, _: Vec<chatty_wasm_runtime::Message>, _: Option<String>)
//! #         -> Result<chatty_wasm_runtime::CompletionResponse, String> { Err("noop".into()) }
//! # }
//! # async fn run() -> anyhow::Result<()> {
//! let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
//! let mut registry = ModuleRegistry::new(provider, ResourceLimits::default())?;
//!
//! // Discover and load all modules under `.chatty/modules/`
//! let report = registry.scan_directory(".chatty/modules")?;
//! println!("Loaded modules: {:?}", report.loaded_names());
//! println!("Failed: {:?}", report.failed);
//! # Ok(())
//! # }
//! ```

pub mod install_record;
pub mod manifest;
mod registry;

pub use hive_client::TrustLevel;
pub use install_record::{INSTALL_RECORD_FILE, InstallRecord};

pub use manifest::{
    ExecutionMode, ModuleCapabilities, ModuleManifest, ModuleProtocols, ModuleResourceLimits,
};
pub use registry::{ModuleHandle, ModuleRegistry, ScanReport};
