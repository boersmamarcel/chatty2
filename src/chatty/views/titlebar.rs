use super::SidebarView;
use gpui::*;
use gpui_component::h_flex;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use gpui_component::{Icon, IconName, Sizable, TitleBar, button::Button};

/// Custom titlebar component for Linux and Windows.
/// On macOS, this renders nothing (uses native traffic lights).
#[derive(IntoElement)]
pub struct AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    sidebar: Entity<SidebarView>,
}

impl AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub fn new(sidebar: Entity<SidebarView>) -> Self {
        Self { sidebar }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    pub fn new(_sidebar: Entity<SidebarView>) -> Self {
        Self {}
    }
}

impl RenderOnce for AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sidebar = self.sidebar.clone();
        let is_collapsed = sidebar.read(cx).is_collapsed();

        h_flex()
            .w_full()
            .child(
                Button::new("toggle-sidebar")
                    .icon(Icon::new(if is_collapsed {
                        IconName::PanelLeftOpen
                    } else {
                        IconName::PanelLeftClose
                    }))
                    .label("")
                    .small()
                    .on_click({
                        let sidebar = sidebar.clone();
                        move |_event, _window, cx| {
                            sidebar.update(cx, |sidebar, cx| {
                                sidebar.toggle_collapsed(cx);
                            });
                        }
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .child(TitleBar::new().on_close_window(|_, window, _cx| {
                        window.remove_window();
                    })),
            )
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // On macOS, return empty - toggle button is rendered as floating overlay in app_view
        div()
    }
}
