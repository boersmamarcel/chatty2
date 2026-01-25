pub mod discovery;
pub mod sync_service;

pub use sync_service::{ensure_default_ollama_provider, sync_ollama_models};
