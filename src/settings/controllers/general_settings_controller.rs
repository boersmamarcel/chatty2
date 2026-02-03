use crate::GENERAL_SETTINGS_REPOSITORY;
use crate::settings::models::GeneralSettingsModel;
use crate::settings::utils::find_theme_variant;
use gpui::{App, AsyncApp, SharedString};
use gpui_component::{ActiveTheme, Theme, ThemeRegistry};
use tracing::{error, warn};

/// Update font size and persist to disk
pub fn update_font_size(cx: &mut App, font_size: f32) {
    // 1. Apply update immediately (optimistic update)
    cx.global_mut::<GeneralSettingsModel>().font_size = font_size;

    // 2. Get updated state for async save
    let settings = cx.global::<GeneralSettingsModel>().clone();

    // 3. Refresh UI immediately (optimistic update)
    cx.refresh_windows();

    // 4. Save async with error handling
    cx.spawn(|_cx: &mut AsyncApp| async move {
        let repo = GENERAL_SETTINGS_REPOSITORY.clone();
        if let Err(e) = repo.save(settings).await {
            error!(error = ?e, "Failed to save general settings, changes will be lost on restart");
        }
    })
    .detach();
}

/// Update selected theme (persistence automatic via observer)
pub fn update_theme(cx: &mut App, base_theme_name: SharedString) {
    // Determine full theme name based on current dark mode
    let is_dark = cx.theme().mode.is_dark();
    let full_theme_name = find_theme_variant(cx, base_theme_name.as_ref(), is_dark);

    // Apply theme - this will trigger the observer in init_themes()
    if let Some(theme) = ThemeRegistry::global(cx)
        .themes()
        .get(&full_theme_name)
        .cloned()
    {
        Theme::global_mut(cx).apply_config(&theme);
        cx.refresh_windows();
    } else {
        warn!(theme_name = %full_theme_name, "Theme not found");
    }
}
