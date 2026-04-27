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
//! let loaded = registry.scan_directory(".chatty/modules")?;
//! println!("Loaded modules: {:?}", loaded);
//! # Ok(())
//! # }
//! ```

pub mod manifest;
mod registry;

pub use manifest::{ModuleCapabilities, ModuleManifest, ModuleProtocols, ModuleResourceLimits};
pub use registry::ModuleRegistry;
