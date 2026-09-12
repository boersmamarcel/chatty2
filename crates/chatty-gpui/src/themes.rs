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

/// Monospace families to try, in order, when the theme's own cannot be
/// loaded. The theme default is already per platform (Menlo / Consolas /
/// DejaVu Sans Mono); these cover a Linux box without DejaVu and the odd
/// macOS install without Menlo. Order is preference, not popularity.
const MONO_FONT_CANDIDATES: &[&str] = &[
    "DejaVu Sans Mono",
    "Menlo",
    "Consolas",
    "Ubuntu Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Courier New",
];

/// Pin the theme's font families to families the platform can actually
/// load (AGE-378).
///
/// gpui caches a family that failed to load as an *error* and clones that
/// error — formatting its message — on every later lookup; `resolve_font`
/// then walks the fallback stack, which on Linux is five more failures
/// before it reaches a font that exists. Every text run of every frame pays
/// that, and a code-heavy turn is thousands of runs: on the AGE-378 fixture
/// it was a quarter of the frame while wheel-scrolling. Resolving once here
/// means every run hits the cached `Ok` instead.
///
/// Idempotent: a family that loads is left alone, so this can run from the
/// theme observer without re-triggering it.
pub(crate) fn resolve_theme_fonts(cx: &mut App) {
    let text_system = cx.text_system().clone();
    // `resolve_font` never fails (it walks the fallbacks and panics past
    // them); the family it actually landed on tells whether the request
    // loaded. An alias (".SystemUIFont") may read back under the face's real
    // name, which just pins the alias to that name — same face, one lookup.
    let resolved_family = |family: &str| -> Option<SharedString> {
        text_system
            .get_font_for_id(text_system.resolve_font(&font(family.to_string())))
            .map(|resolved| resolved.family)
    };
    let loads = |family: &str| resolved_family(family).is_some_and(|resolved| resolved == family);

    let (ui_family, mono_family) = {
        let theme = cx.theme();
        (theme.font_family.clone(), theme.mono_font_family.clone())
    };

    // The UI family: whatever gpui's own fallback walk would have landed on,
    // recorded so the walk happens once instead of per run.
    let ui_resolved = (!loads(&ui_family))
        .then(|| resolved_family(&ui_family))
        .flatten()
        .filter(|resolved| *resolved != ui_family);

    // The mono family: gpui's fallbacks are proportional fonts, so code
    // would lose its alignment — try real monospace families first.
    let mono_resolved = (!loads(&mono_family))
        .then(|| {
            MONO_FONT_CANDIDATES
                .iter()
                .find(|candidate| loads(candidate))
                .map(|candidate| SharedString::from(*candidate))
                .or_else(|| resolved_family(&mono_family))
        })
        .flatten()
        .filter(|resolved| *resolved != mono_family);

    if ui_resolved.is_none() && mono_resolved.is_none() {
        return;
    }
    if let Some(family) = &ui_resolved {
        info!(requested = %ui_family, resolved = %family, "UI font family is not installed; pinned to a loadable one");
    }
    if let Some(family) = &mono_resolved {
        info!(requested = %mono_family, resolved = %family, "Mono font family is not installed; pinned to a loadable one");
    }
    let theme = Theme::global_mut(cx);
    if let Some(family) = ui_resolved {
        theme.font_family = family;
    }
    if let Some(family) = mono_resolved {
        theme.mono_font_family = family;
    }
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
        // A theme pack may name fonts this machine does not have (AGE-378).
        resolve_theme_fonts(cx);

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

    // The observer only fires on later changes; the baked-in default theme
    // is already in place, so resolve it now.
    resolve_theme_fonts(cx);
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
