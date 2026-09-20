use crate::assets::CustomIcon;
use crate::auto_updater::{AutoUpdateStatus, AutoUpdater};
use crate::chatty::views::SidebarView;
use crate::chatty::views::footer::{
    AgentIndicatorView, AutoUpdateView, ErrorIndicatorView, FetchIndicatorView, McpIndicatorView,
    NetworkIndicatorView, TokenContextBarView, ToolsIndicatorView,
};
use crate::settings::models::general_model::SidebarMode;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::{
    Icon, Selectable, Sizable,
    button::{Button, ButtonVariants},
};

#[derive(IntoElement)]
pub struct StatusFooterView {
    sidebar: Entity<SidebarView>,
}

impl StatusFooterView {
    pub fn new(sidebar: Entity<SidebarView>) -> Self {
        Self { sidebar }
    }
}

impl RenderOnce for StatusFooterView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let files_mode = self.sidebar.read(cx).is_files_mode();
        let sidebar = self.sidebar.clone();
        div()
            .h(px(24.0))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(px(8.0))
            .bg(cx.theme().background)
            .border_t_1()
            .border_color(cx.theme().border)
            // Left side: Chats/Files toggle (AGE-480) + errors/warnings + auto-updater
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("sidebar-mode-chats")
                            .ghost()
                            .xsmall()
                            .selected(!files_mode)
                            .icon(Icon::new(CustomIcon::MessageSquare))
                            .tooltip("Chats")
                            .on_click({
                                let sidebar = sidebar.clone();
                                move |_event, _window, cx| {
                                    sidebar.update(cx, |sidebar, cx| {
                                        sidebar.set_mode(SidebarMode::Chats, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("sidebar-mode-files")
                            .ghost()
                            .xsmall()
                            .selected(files_mode)
                            .icon(Icon::new(CustomIcon::FolderTree))
                            .tooltip("Files")
                            .on_click({
                                let sidebar = sidebar.clone();
                                move |_event, _window, cx| {
                                    sidebar.update(cx, |sidebar, cx| {
                                        sidebar.set_mode(SidebarMode::Files, cx);
                                    });
                                }
                            }),
                    )
                    .child(ErrorIndicatorView::new().on_click(move |window, cx| {
                        // Open error log dialog as inline overlay
                        crate::chatty::views::ErrorLogDialog::open(window, cx);
                    }))
                    .child(AutoUpdateView::new().on_click(move |_window, cx| {
                        // Determine which action to take based on current status
                        let status = cx.global::<AutoUpdater>().status().clone();

                        match status {
                            AutoUpdateStatus::Idle => {
                                let updater = cx.global::<AutoUpdater>().clone();
                                updater.check_for_update(cx);
                            }
                            AutoUpdateStatus::Ready(..) => {
                                let mut updater = cx.global::<AutoUpdater>().clone();
                                updater.install_and_restart(cx);
                            }
                            AutoUpdateStatus::Error(_) => {
                                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                    updater.dismiss_error();
                                });
                            }
                            _ => {
                                // Do nothing for Checking, Downloading states
                            }
                        }
                    })),
            )
            // Right side: Tools + MCP
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(TokenContextBarView::new())
                    .child(FetchIndicatorView::new())
                    .child(NetworkIndicatorView::new())
                    .child(ToolsIndicatorView::new())
                    .child(McpIndicatorView::new())
                    .child(AgentIndicatorView::new()),
            )
    }
}
