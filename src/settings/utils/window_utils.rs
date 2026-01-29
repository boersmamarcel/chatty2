use gpui::*;

/// Returns platform-specific WindowOptions for the main Chatty window.
///
/// Platform-specific behavior:
/// - macOS: Transparent titlebar with native decorations
/// - Windows: Transparent titlebar with client-side decorations
/// - Linux: Non-transparent titlebar with client-side decorations
pub fn get_main_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: Some(get_titlebar_options()),
        window_decorations: get_window_decorations(),
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Point::default(),
            size: size(px(1000.0), px(600.0)),
        })),
        ..Default::default()
    }
}

/// Returns platform-specific titlebar options.
///
/// - macOS/Windows: Transparent titlebar with traffic light positioning at None
/// - Linux: Non-transparent titlebar
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn get_titlebar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: Some("Chatty".into()),
        appears_transparent: true,
        traffic_light_position: None,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn get_titlebar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: Some("Chatty".into()),
        appears_transparent: false,
        traffic_light_position: None,
    }
}

/// Returns platform-specific window decorations.
///
/// - Linux/Windows: Client-side window decorations (app controls the window chrome)
/// - macOS: None (use native macOS decorations)
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn get_window_decorations() -> Option<WindowDecorations> {
    Some(WindowDecorations::Client)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn get_window_decorations() -> Option<WindowDecorations> {
    None
}
