use crate::chatty::controllers::ChattyApp;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::*;
use gpui_component::{ActiveTheme as _, Root};

impl Render for ChattyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);

        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .child(
                // Sidebar - left panel
                self.sidebar_view.clone(),
            )
            .child(
                // Chat view - right panel
                self.chat_view.clone(),
            )
            .children(dialog_layer)
    }
}
