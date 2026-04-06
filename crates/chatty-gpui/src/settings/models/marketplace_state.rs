use chatty_core::hive::models::{Category, ModuleMetadata};
use gpui::Global;

/// Ephemeral UI state for the Extensions marketplace browser.
/// Not persisted — rebuilt every time the page is opened.
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
}

impl MarketplaceState {
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

    pub fn set_results(&mut self, items: Vec<ModuleMetadata>, total: i64, page: i64) {
        self.loading = false;
        self.error = None;
        self.search_results = items;
        self.total = total;
        self.page = page;
    }

    pub fn has_more_pages(&self, per_page: i64) -> bool {
        self.page * per_page < self.total
    }
}

impl Global for MarketplaceState {}
