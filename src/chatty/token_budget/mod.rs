//! Token budget tracking for the context window.
//!
//! This module provides accurate, non-blocking token counting for the LLM context
//! window, replacing the legacy `chars / 4` heuristic used in v1.
//!
//! # Architecture
//!
//! ```text
//! run_llm_stream()
//!   → manager::gather_snapshot_inputs()     (GPUI thread — reads globals, warms cache)
//!   → manager::compute_snapshot_background() (tokio::spawn_blocking — BPE counting)
//!   → GlobalTokenBudget::publish()           (watch::Sender — O(1), non-blocking)
//!   → TokenContextBarView::render()
//!       reads receiver.borrow().clone() on every repaint
//! ```
//!
//! # Key types
//!
//! - [`TokenBudgetSnapshot`] — point-in-time breakdown of token usage by component
//! - [`ContextStatus`]       — traffic-light status derived from utilization ratio
//! - [`GlobalTokenBudget`]   — GPUI global owning the watch channel and the cache

pub mod cache;
pub mod counter;
pub mod manager;
pub mod snapshot;
pub mod summarizer;

// ── Snapshot types (used by token_context_bar_view and app_controller) ────────
pub use snapshot::{ContextStatus, TokenBudgetSnapshot};

// ── Manager / watch channel ───────────────────────────────────────────────────
pub use manager::{
    GlobalTokenBudget, check_pressure, compute_snapshot_background, extract_user_message_text,
    gather_snapshot_inputs,
};

// ── Summarizer ────────────────────────────────────────────────────────────────
pub use summarizer::summarize_oldest_half;
