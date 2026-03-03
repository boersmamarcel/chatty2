// Forward-looking public API: many items here are defined for use by future
// features (summarizer, pressure events, settings integration) and are not
// yet called from the rest of the codebase.  Suppress dead_code / unused_import
// lints for the entire token_budget module tree so clippy stays clean without
// requiring per-item annotations on every future-use item.
#![allow(dead_code, unused_imports)]

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
//! - [`ContextPressureEvent`]— emitted when utilization crosses 70% / 90% thresholds
//! - [`TokenCounter`]        — provider-aware tiktoken-rs BPE wrapper
//! - [`CachedTokenCounts`]   — hash-keyed cache for preamble and tool definition tokens
//! - [`GlobalTokenBudget`]   — GPUI global owning the watch channel and the cache

pub mod cache;
pub mod counter;
pub mod manager;
pub mod snapshot;
pub mod summarizer;

// ── Snapshot types ────────────────────────────────────────────────────────────
pub use snapshot::{ComponentFractions, ContextPressureEvent, ContextStatus, TokenBudgetSnapshot};

// ── Tokenizer ─────────────────────────────────────────────────────────────────
pub use counter::{Encoding, TokenCounter};

// ── Cache ─────────────────────────────────────────────────────────────────────
pub use cache::{CachedTokenCounts, build_tool_hint};

// ── Manager / watch channel ───────────────────────────────────────────────────
pub use manager::{
    GlobalTokenBudget, SnapshotInputs, check_pressure, compute_snapshot_background,
    extract_user_message_text, gather_snapshot_inputs,
};

// ── Summarizer ────────────────────────────────────────────────────────────────
pub use summarizer::{SummarizationResult, summarize_oldest_half};
