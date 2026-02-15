use crate::auto_updater::{AutoUpdateStatus, AutoUpdater};
use crate::chatty::views::footer::{AutoUpdateView, ErrorIndicatorView, McpIndicatorView};
use gpui::*;
use gpui_component::ActiveTheme as _;

#[derive(IntoElement)]
pub struct StatusFooterView;

impl StatusFooterView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for StatusFooterView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
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
            // Left side: errors/warnings + auto-updater
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
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
            // Right side: MCP tools
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(McpIndicatorView::new()),
            )
    }
}
