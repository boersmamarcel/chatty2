use crate::chatty::controllers::ChattyApp;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::*;
use gpui_component::ActiveTheme as _;

impl Render for ChattyApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .child(format!(
                "Hello, Chatty! Font-size:{}",
                cx.global::<GeneralSettingsModel>().font_size
            ))
    }
}
