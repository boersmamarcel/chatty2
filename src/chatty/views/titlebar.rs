use gpui::*;
use gpui_component::TitleBar;

/// Custom titlebar component for Linux and Windows.
/// On macOS, this renders nothing (uses native traffic lights).
#[derive(IntoElement)]
pub struct AppTitleBar;

impl AppTitleBar {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        TitleBar::new().on_close_window(|_, window, _cx| {
            window.remove_window();
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // On macOS, return empty element - uses native window controls
        div()
    }
}
