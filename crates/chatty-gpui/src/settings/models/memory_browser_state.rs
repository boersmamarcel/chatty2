use chatty_core::services::memory_service::{MemoryHit, MemoryStats};
use gpui::Global;

/// Ephemeral UI state for the Memory Browser panel in Settings > Memory.
/// Not persisted — rebuilt whenever the panel is opened or searched.
#[derive(Clone, Default)]
pub struct MemoryBrowserState {
    /// Currently displayed memory entries.
    pub entries: Vec<MemoryHit>,
    /// Whether a load/search is in progress.
    pub loading: bool,
    /// Error message from the last operation, if any.
    pub error: Option<String>,
    /// The last search query (empty = list all).
    pub query: String,
    /// Cached memory store statistics.
    pub stats: Option<MemoryStats>,
    /// Index of the currently expanded entry, if any.
    pub expanded_index: Option<usize>,
}

impl MemoryBrowserState {
    pub fn set_loading(&mut self, query: &str) {
        self.loading = true;
        self.query = query.to_string();
        self.error = None;
    }

    pub fn set_entries(&mut self, entries: Vec<MemoryHit>) {
        self.loading = false;
        self.entries = entries;
        self.error = None;
        // Reset expanded state when results change
        self.expanded_index = None;
    }

    pub fn set_error(&mut self, msg: String) {
        self.loading = false;
        self.error = Some(msg);
    }

    pub fn set_stats(&mut self, stats: MemoryStats) {
        self.stats = Some(stats);
    }

    pub fn toggle_expanded(&mut self, index: usize) {
        if self.expanded_index == Some(index) {
            self.expanded_index = None;
        } else {
            self.expanded_index = Some(index);
        }
    }
}

impl Global for MemoryBrowserState {}
