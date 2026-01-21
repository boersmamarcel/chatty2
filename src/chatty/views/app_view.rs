use crate::chatty::controllers::ChattyApp;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::*;
use gpui::prelude::FluentBuilder;
use gpui_component::ActiveTheme as _;
use gpui_component::scroll::ScrollableElement;

impl Render for ChattyApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let font_size = cx.global::<GeneralSettingsModel>().font_size;

        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(theme.background)
            .child(
                // Sidebar
                self.render_sidebar(cx, &theme, font_size),
            )
            .child(
                // Main content area
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .bg(theme.background)
                    .child(
                        div()
                            .text_size(px(font_size * 1.5))
                            .text_color(theme.muted_foreground)
                            .child("Select a conversation to start chatting"),
                    ),
            )
    }
}

impl ChattyApp {
    fn render_sidebar(
        &self,
        _cx: &Context<Self>,
        theme: &gpui_component::Theme,
        font_size: f32,
    ) -> Div {
        div()
            .w(px(280.0))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.secondary)
            .border_r_1()
            .border_color(theme.border)
            .child(
                // Header
                div()
                    .w_full()
                    .h(px(60.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(font_size * 1.2))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("Conversations"),
                    )
                    .child(
                        // New conversation button
                        div()
                            .px_3()
                            .py_2()
                            .rounded(px(6.0))
                            .bg(theme.primary)
                            .text_color(theme.background)
                            .text_size(px(font_size))
                            .cursor_pointer()
                            .child("+"),
                    ),
            )
            .child(
                // Conversations list
                div()
                    .flex_1()
                    .w_full()
                    .overflow_y_scrollbar()
                    .children(self.conversations.iter().map(|conversation| {
                        let is_selected = self
                            .selected_conversation_id
                            .as_ref()
                            .map(|id| id == &conversation.id)
                            .unwrap_or(false);

                        div()
                            .w_full()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .when(is_selected, |s: Div| s.bg(theme.muted))
                            .hover(|style: StyleRefinement| style.bg(theme.muted))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        // Title
                                        div()
                                            .text_size(px(font_size))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child(conversation.title.clone()),
                                    )
                                    .child(
                                        // Preview
                                        div()
                                            .text_size(px(font_size * 0.85))
                                            .text_color(theme.muted_foreground)
                                            .child(conversation.get_preview()),
                                    )
                                    .child(
                                        // Timestamp
                                        div()
                                            .text_size(px(font_size * 0.75))
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                conversation
                                                    .updated_at
                                                    .format("%Y-%m-%d %H:%M")
                                                    .to_string(),
                                            ),
                                    ),
                            )
                    })),
            )
    }
}
