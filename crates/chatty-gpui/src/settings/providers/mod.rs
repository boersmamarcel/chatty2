pub mod ollama;
pub mod openrouter;

pub use ollama::{ensure_default_ollama_provider, sync_ollama_models};
