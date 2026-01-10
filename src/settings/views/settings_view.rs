use crate::settings::controllers::SettingsView;
use gpui::*;

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .child("Hello, Settings!")
    }
}
