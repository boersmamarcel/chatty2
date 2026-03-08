//! Token budget tracking for the context window.

pub mod cache;
pub mod counter;
pub mod snapshot;
pub mod summarizer;

pub use snapshot::{ContextStatus, TokenBudgetSnapshot};
pub use summarizer::summarize_oldest_half;
