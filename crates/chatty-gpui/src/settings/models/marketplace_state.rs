use std::collections::HashMap;

use chatty_core::hive::models::{AgentSpecListing, Category, ModuleMetadata};
use chatty_core::team_install::TeamRecord;
use gpui::Global;

/// Which listing the marketplace shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarketplaceTab {
    #[default]
    Plugins,
    /// Published agent teams (MK-T2, AGE-841).
    Teams,
}

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
    /// The registry rejected the refresh token, so the Hive session ended
    /// without the user signing out. Cleared by the next sign-in.
    pub signed_out_of_hive: bool,
    pub tab: MarketplaceTab,
    /// The last team search's results.
    pub team_results: Vec<AgentSpecListing>,
    /// The teams installed on this machine, as last read from disk.
    pub installed_teams: Vec<TeamRecord>,
    /// The team being fetched and verified, while it is.
    pub installing_team: Option<String>,
    /// What the last team install or uninstall did.
    pub team_notice: Option<String>,
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
        self.downloading
            .insert(name.to_string(), progress.clamp(0.0, 1.0));
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
