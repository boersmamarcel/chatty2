pub mod ollama;
pub mod openrouter;

pub use ollama::{ensure_default_ollama_provider, resync_ollama_models, sync_ollama_models};
