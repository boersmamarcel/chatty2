use crate::chatty::controllers::ChattyApp;
use gpui::*;

impl Render for ChattyApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x1e1e1e)).child("Hello, Chatty!")
    }
}
