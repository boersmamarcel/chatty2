use gpui::{App, SharedString};
use gpui_component::{ActiveTheme, ThemeRegistry};
use std::collections::HashSet;

/// Extract the base theme name by removing " Light" or " Dark" suffixes.
pub fn extract_base_theme_name(full_name: &str) -> String {
    if let Some(stripped) = full_name.strip_suffix(" Light") {
        stripped.to_string()
    } else if let Some(stripped) = full_name.strip_suffix(" Dark") {
        stripped.to_string()
    } else {
        full_name.to_string()
    }
}

/// Find the appropriate theme variant based on base name and dark mode preference.
/// Returns the full theme name (e.g., "Ayu Dark" or "Ayu Light").
pub fn find_theme_variant(cx: &App, base_name: &str, is_dark: bool) -> SharedString {
    let all_themes = ThemeRegistry::global(cx).themes();

    // Try common patterns: "Base Dark", "Base Light", or just "Base"
    let candidates = if is_dark {
        vec![format!("{} Dark", base_name), base_name.to_string()]
    } else {
        vec![format!("{} Light", base_name), base_name.to_string()]
    };

    // Find first matching theme
    for candidate in candidates {
        let candidate_shared: SharedString = candidate.into();
        if all_themes.contains_key(&candidate_shared) {
            return candidate_shared;
        }
    }

    // Fallback to base name
    base_name.to_string().into()
}

/// Get all unique base theme names from the registry, sorted alphabetically.
/// Returns tuples of (value, label) for use in dropdowns.
pub fn get_all_base_theme_names(cx: &App) -> Vec<(SharedString, SharedString)> {
    let all_themes: Vec<SharedString> =
        ThemeRegistry::global(cx).themes().keys().cloned().collect();

    // Extract unique base theme names
    let mut theme_bases: HashSet<String> = HashSet::new();
    for theme_name in &all_themes {
        let base_name = extract_base_theme_name(theme_name.as_ref());
        theme_bases.insert(base_name);
    }

    // Convert to sorted Vec for dropdown
    let mut theme_options: Vec<(SharedString, SharedString)> = theme_bases
        .into_iter()
        .map(|name| {
            let shared: SharedString = name.into();
            (shared.clone(), shared)
        })
        .collect();

    theme_options.sort_by(|a, b| a.0.as_ref().cmp(b.0.as_ref()));
    theme_options
}

/// Get the appropriate syntect theme name based on GPUI's current theme mode.
/// Returns a theme name that syntect can use for syntax highlighting.
pub fn get_syntect_theme_name(cx: &App) -> &'static str {
    if cx.theme().mode.is_dark() {
        "Solarized (dark)"
    } else {
        "Solarized (light)"
    }
}
