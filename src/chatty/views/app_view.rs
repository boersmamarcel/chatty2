use crate::chatty::controllers::ChattyApp;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::*;
use gpui_component::{ActiveTheme as _, Root};

impl Render for ChattyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);

        div()
            .size_full()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .child(format!(
                "Hello, Chatty! Font-size:{}",
                cx.global::<GeneralSettingsModel>().font_size
            ))
            .children(dialog_layer)
    }
}
