//! Theme discovery, loading, and live application for the desktop binary.
//!
//! `init_themes` scans the bundled `themes/` directory at startup and
//! installs them into GPUI's theme registry; `apply_theme_from_settings`
//! re-applies the user's saved theme + dark-mode preference whenever the
//! global settings change.

use super::*;

pub(crate) fn get_themes_dir() -> PathBuf {
    // Check CHATTY_DATA_DIR environment variable (set by AppImage)
    if let Ok(data_dir) = std::env::var("CHATTY_DATA_DIR") {
        let themes_path = PathBuf::from(data_dir).join("themes");
        if themes_path.exists() {
            return themes_path;
        }
    }

    // Try to find themes directory relative to the executable
    #[cfg(target_os = "macos")]
    {
        // On macOS, check in the app bundle's Resources directory
        if let Ok(exe_path) = std::env::current_exe()
            && let Some(app_bundle) = exe_path
                .ancestors()
                .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        {
            let resources_themes = app_bundle.join("Contents/Resources/themes");
            if resources_themes.exists() {
                return resources_themes;
            }
        }
    }

    // Default to ./themes for development and Linux/Windows
    PathBuf::from("./themes")
}

pub(crate) fn init_themes(cx: &mut App) {
    let themes_dir = get_themes_dir();
    info!(themes_dir = ?themes_dir, "Loading themes from directory");

    // Just watch themes directory to load the registry
    if let Err(err) = ThemeRegistry::watch_dir(themes_dir, cx, |_cx| {
        // Empty callback - just loading themes into registry
    }) {
        warn!(error = ?err, "Failed to watch themes directory");
    }

    // Observe theme changes and persist base theme name + dark mode to GeneralSettingsModel
    // Only persist after initialization is complete to avoid overwriting saved preferences
    cx.observe_global::<Theme>(|cx| {
        // Skip saving during initialization - settings haven't been loaded yet
        if !THEME_INIT_COMPLETE.load(Ordering::SeqCst) {
            debug!("Skipping theme save during initialization");
            return;
        }

        let full_theme_name = cx.theme().theme_name().to_string();
        let is_dark = cx.theme().mode.is_dark();

        // Extract base theme name using shared utility
        let base_theme_name = settings::utils::extract_base_theme_name(&full_theme_name);

        // Update model with base name and dark mode
        {
            let settings = cx.global_mut::<settings::models::general_model::GeneralSettingsModel>();
            settings.theme_name = Some(base_theme_name);
            settings.dark_mode = Some(is_dark);
        }

        // Save async
        let settings = cx
            .global::<settings::models::general_model::GeneralSettingsModel>()
            .clone();
        cx.spawn(|_cx: &mut AsyncApp| async move {
            let repo = chatty_core::general_settings_repository();
            if let Err(e) = repo.save(settings).await {
                warn!(error = ?e, "Failed to save theme preference");
            }
        })
        .detach();
    })
    .detach();

    cx.refresh_windows();
}

/// Apply theme from saved settings (called after settings are loaded from JSON)
pub(crate) fn apply_theme_from_settings(cx: &mut App) {
    let base_theme_name = cx
        .global::<settings::models::general_model::GeneralSettingsModel>()
        .theme_name
        .clone()
        .unwrap_or_else(|| "Ayu".to_string());

    let is_dark = cx
        .global::<settings::models::general_model::GeneralSettingsModel>()
        .dark_mode
        .unwrap_or(false);

    info!(
        theme = %base_theme_name,
        dark_mode = is_dark,
        "Applying theme from saved settings"
    );

    // Find the appropriate theme variant using shared utility
    let full_theme_name = settings::utils::find_theme_variant(cx, &base_theme_name, is_dark);

    if let Some(theme) = ThemeRegistry::global(cx)
        .themes()
        .get(&full_theme_name)
        .cloned()
    {
        // Set the mode first
        let mode = if is_dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        Theme::global_mut(cx).mode = mode;

        // Then apply the theme
        Theme::global_mut(cx).apply_config(&theme);
        cx.refresh_windows();

        info!(theme = %full_theme_name, "Theme applied successfully");
    } else {
        warn!(
            theme = %full_theme_name,
            "Theme not found in registry, keeping default"
        );
    }

    // Mark initialization complete - now the observer can save user changes
    THEME_INIT_COMPLETE.store(true, Ordering::SeqCst);
    debug!("Theme initialization complete, observer now active");
}

