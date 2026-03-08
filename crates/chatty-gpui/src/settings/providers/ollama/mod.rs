// Re-export discovery from chatty-core
pub use chatty_core::settings::providers::ollama::discovery;

// Local gpui-specific module
pub mod sync_service;

pub use sync_service::{ensure_default_ollama_provider, sync_ollama_models};
