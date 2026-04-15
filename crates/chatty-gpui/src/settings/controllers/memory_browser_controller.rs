use crate::settings::models::MemoryBrowserState;
use chatty_core::services::MemoryService;
use gpui::App;
use tracing::{error, warn};

/// Maximum number of entries to load when listing all memories (no query).
const LIST_ALL_LIMIT: usize = 200;
/// Maximum number of entries to return for a search query.
const SEARCH_LIMIT: usize = 50;

/// Load memories from the store and update `MemoryBrowserState`.
///
/// If `query` is empty, up to `LIST_ALL_LIMIT` entries are fetched.
/// If `query` is non-empty, a full-text search is performed (up to `SEARCH_LIMIT`).
pub fn load_memories(query: String, cx: &mut App) {
    let memory_service = cx.try_global::<MemoryService>().cloned();
    let Some(service) = memory_service else {
        return;
    };

    if !cx.has_global::<MemoryBrowserState>() {
        cx.set_global(MemoryBrowserState::default());
    }

    cx.global_mut::<MemoryBrowserState>().set_loading(&query);
    cx.refresh_windows();

    let limit = if query.is_empty() {
        LIST_ALL_LIMIT
    } else {
        SEARCH_LIMIT
    };

    cx.spawn(async move |cx| {
        let result = service.list_memories(&query, limit).await;
        cx.update(|cx| {
            match result {
                Ok(entries) => cx.global_mut::<MemoryBrowserState>().set_entries(entries),
                Err(e) => {
                    warn!(error = ?e, "Failed to load memories for browser");
                    cx.global_mut::<MemoryBrowserState>()
                        .set_error(e.to_string());
                }
            }
            cx.refresh_windows();
        })
        .map_err(|e| warn!(error = ?e, "Failed to update MemoryBrowserState after load"))
        .ok();
    })
    .detach();
}

/// Fetch and cache memory store statistics (entry count, file size).
pub fn load_stats(cx: &mut App) {
    let memory_service = cx.try_global::<MemoryService>().cloned();
    let Some(service) = memory_service else {
        return;
    };

    if !cx.has_global::<MemoryBrowserState>() {
        cx.set_global(MemoryBrowserState::default());
    }

    cx.spawn(async move |cx| match service.stats().await {
        Ok(stats) => {
            cx.update(|cx| {
                cx.global_mut::<MemoryBrowserState>().set_stats(stats);
                cx.refresh_windows();
            })
            .map_err(|e| warn!(error = ?e, "Failed to update stats in MemoryBrowserState"))
            .ok();
        }
        Err(e) => {
            error!(error = ?e, "Failed to fetch memory stats");
        }
    })
    .detach();
}

/// Toggle expansion of a memory entry by index.
pub fn toggle_entry(index: usize, cx: &mut App) {
    if !cx.has_global::<MemoryBrowserState>() {
        return;
    }
    cx.global_mut::<MemoryBrowserState>().toggle_expanded(index);
    cx.refresh_windows();
}
