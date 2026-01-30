use crate::chatty::controllers::ChattyApp;
use crate::chatty::views::AppTitleBar;
use crate::chatty::views::footer::StatusFooterView;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::*;
use gpui_component::{ActiveTheme as _, Root};

impl Render for ChattyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .child(
                // Custom titlebar for Linux/Windows (empty on macOS)
                AppTitleBar::new(),
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
            .children(dialog_layer)
    }
}
