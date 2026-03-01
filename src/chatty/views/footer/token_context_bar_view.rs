use crate::chatty::models::conversations_store::ConversationsStore;
use crate::settings::models::models_store::ModelsModel;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::tooltip::Tooltip;

#[derive(IntoElement, Default)]
pub struct TokenContextBarView;

impl TokenContextBarView {
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for TokenContextBarView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = cx.global::<ConversationsStore>();
        let data = store.active_id().and_then(|active_id| {
            let conv = store.get_conversation(active_id)?;
            let current_tokens = conv
                .token_usage()
                .message_usages
                .last()
                .map(|u| u.input_tokens)
                .unwrap_or(0);
            let model_id = conv.model_id().to_string();
            let max_context_window = cx
                .global::<ModelsModel>()
                .get_model(&model_id)
                .and_then(|m| m.max_context_window)
                .map(|v| v as u32)?;
            Some((current_tokens, max_context_window))
        });

        let Some((current_tokens, max_context_window)) = data else {
            return div().id("token-context-bar-hidden");
        };

        let pct = (current_tokens as f32 / max_context_window as f32 * 100.0).clamp(0.0, 100.0);

        let bar_color = if pct >= 85.0 {
            rgb(0xEF4444) // Red-500
        } else if pct >= 60.0 {
            rgb(0xF59E0B) // Amber-500
        } else {
            rgb(0x22C55E) // Green-500
        };

        let tooltip_text = format!(
            "{} / {} tokens ({:.0}%)",
            current_tokens, max_context_window, pct
        );

        const BAR_WIDTH: f32 = 80.0;

        div()
            .id("token-context-bar")
            .flex()
            .items_center()
            .px_1()
            .child(
                div()
                    .w(px(BAR_WIDTH))
                    .h(px(6.0))
                    .rounded_sm()
                    .bg(cx.theme().border)
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(BAR_WIDTH * pct / 100.0))
                            .h_full()
                            .rounded_sm()
                            .bg(bar_color),
                    ),
            )
            .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
    }
}
