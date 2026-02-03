use crate::chatty::controllers::ChattyApp;
use crate::chatty::views::AppTitleBar;
use crate::chatty::views::footer::StatusFooterView;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme as _, Icon, IconName, Root, Sizable, button::Button};

impl Render for ChattyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let sidebar = self.sidebar_view.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .relative() // Enable absolute positioning for floating button
            .child(
                // Custom titlebar with toggle button
                AppTitleBar::new(self.sidebar_view.clone()),
            )
            .child(
                // Content area - existing panels
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(
                        // Sidebar - left panel
                        self.sidebar_view.clone(),
                    )
                    .child(
                        // Chat view - right panel
                        self.chat_view.clone(),
                    ),
            )
            .child(
                // Footer bar
                StatusFooterView::new(),
            )
            // Floating toggle button for macOS (rendered last = on top)
            .when(cfg!(target_os = "macos"), |this| {
                let is_collapsed = sidebar.read(cx).is_collapsed();
                this.child(
                    div().absolute().top(px(8.)).left(px(80.)).child(
                        Button::new("toggle-sidebar-floating")
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
                    ),
                )
            })
            .children(dialog_layer)
    }
}
