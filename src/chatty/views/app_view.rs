use crate::chatty::controllers::ChattyApp;
use gpui::*;
use gpui_component::ActiveTheme as _;

impl Render for ChattyApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .child("Hello, Chatty!")
    }
}
