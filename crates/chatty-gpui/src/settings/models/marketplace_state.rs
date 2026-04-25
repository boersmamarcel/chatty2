use std::collections::HashMap;

use chatty_core::hive::models::{Category, ModuleMetadata};
use gpui::Global;

/// Ephemeral UI state for the Extensions marketplace browser.
/// Not persisted — rebuilt every time the page is opened.
#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct MarketplaceState {
    pub search_query: String,
    pub search_results: Vec<ModuleMetadata>,
    pub categories: Vec<Category>,
    pub selected_category: Option<String>,
    pub page: i64,
    pub total: i64,
    pub loading: bool,
    pub error: Option<String>,
    pub featured: Vec<ModuleMetadata>,
    /// Per-module download progress: module name → 0.0 … 1.0.
    /// Only present while a download is in flight.
    pub downloading: HashMap<String, f32>,
}

impl MarketplaceState {
    #[allow(dead_code)]
    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_results.clear();
        self.page = 1;
        self.total = 0;
        self.error = None;
    }

    pub fn set_loading(&mut self) {
        self.loading = true;
        self.error = None;
    }

    pub fn set_error(&mut self, msg: String) {
        self.loading = false;
        self.error = Some(msg);
    }

    /// Record that `name` is being downloaded at `progress` (0.0 – 1.0).
    pub fn set_download_progress(&mut self, name: &str, progress: f32) {
        self.downloading.insert(name.to_string(), progress.clamp(0.0, 1.0));
    }

    /// Remove the download-in-progress entry for `name` (called on success or failure).
    pub fn clear_download_progress(&mut self, name: &str) {
        self.downloading.remove(name);
    }

    /// Return the current download progress for `name`, or `None` if not downloading.
    pub fn download_progress(&self, name: &str) -> Option<f32> {
        self.downloading.get(name).copied()
    }

    pub fn set_results(&mut self, items: Vec<ModuleMetadata>, total: i64, page: i64) {
        self.loading = false;
        self.error = None;
        self.search_results = items;
        self.total = total;
        self.page = page;
    }

    #[allow(dead_code)]
    pub fn has_more_pages(&self, per_page: i64) -> bool {
        self.page * per_page < self.total
    }
}

impl Global for MarketplaceState {}
