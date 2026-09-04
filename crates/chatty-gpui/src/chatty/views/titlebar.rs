use super::{ChatView, SidebarView};
use gpui::*;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use crate::chatty::views::transcript::ArtifactMode;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use gpui_component::{
    Icon, IconName, Sizable, TitleBar,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    menu::{AppMenuBar, PopupMenuItem},
};

/// Custom titlebar component for Linux and Windows.
/// On macOS, this renders nothing (uses native traffic lights).
#[derive(IntoElement)]
pub struct AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    sidebar: Entity<SidebarView>,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    chat_view: Entity<ChatView>,
}

impl AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub fn new(sidebar: Entity<SidebarView>, chat_view: Entity<ChatView>) -> Self {
        Self { sidebar, chat_view }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    pub fn new(_sidebar: Entity<SidebarView>, _chat_view: Entity<ChatView>) -> Self {
        Self {}
    }
}

impl RenderOnce for AppTitleBar {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sidebar = self.sidebar.clone();
        let is_collapsed = sidebar.read(cx).is_collapsed();
        let chat_view = self.chat_view.clone();
        let is_artifact_open =
            chat_view.read(cx).artifact_view().read(cx).mode != ArtifactMode::Closed;
        let app_menu_bar = AppMenuBar::new(window, cx);

        h_flex()
            .w_full()
            .child(
                Button::new("toggle-sidebar")
                    .ghost()
                    .icon(Icon::new(if is_collapsed {
                        IconName::PanelLeftOpen
                    } else {
                        IconName::PanelLeftClose
                    }))
                    .label("")
                    .small()
                    .w(px(30.))
                    .h(px(28.))
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
                Button::new("search-conversations")
                    .icon(Icon::new(IconName::Search))
                    .label("")
                    .small()
                    .tooltip("Search conversations")
                    .on_click(|_event, window, cx| {
                        super::SearchConversationsDialog::open(window, cx);
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .child(TitleBar::new().child(app_menu_bar).on_close_window(
                        |_, window, _cx| {
                            window.remove_window();
                        },
                    )),
            )
            .child(
                // Unfold/fold the artifact panel; the caret opens a small
                // picker for manually starting one — "Browser" for now.
                DropdownButton::new("toggle-artifact")
                    .small()
                    .button(
                        Button::new("toggle-artifact-main")
                            .ghost()
                            .icon(Icon::new(if is_artifact_open {
                                IconName::PanelRightClose
                            } else {
                                IconName::PanelRightOpen
                            }))
                            .label("")
                            .small()
                            .tooltip("Toggle artifact panel")
                            .on_click({
                                let chat_view = chat_view.clone();
                                move |_event, _window, cx| {
                                    chat_view.update(cx, |view, cx| {
                                        view.toggle_artifact_panel(cx);
                                    });
                                }
                            }),
                    )
                    .dropdown_menu({
                        let chat_view = chat_view.clone();
                        move |menu, _window, _cx| {
                            menu.item(PopupMenuItem::new("Browser").on_click({
                                let chat_view = chat_view.clone();
                                move |_event, _window, cx| {
                                    chat_view.update(cx, |view, cx| {
                                        view.open_manual_browser(cx);
                                    });
                                }
                            }))
                        }
                    }),
            )
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // On macOS, return empty - toggle button is rendered as floating overlay in app_view
        div()
    }
}
