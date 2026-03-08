use super::syntax_highlighter;
use crate::assets::CustomIcon;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable};
use std::ops::Range;

/// A code block component with syntax highlighting and a copy button
#[derive(IntoElement, Clone)]
pub struct CodeBlockComponent {
    language: Option<String>,
    code: String,
    block_index: usize,
    /// Pre-computed highlight styles. If Some, skip highlight_code() in render.
    pre_highlighted: Option<Vec<(Range<usize>, HighlightStyle)>>,
}

impl CodeBlockComponent {
    #[allow(dead_code)]
    pub fn new(language: Option<String>, code: String, block_index: usize) -> Self {
        Self {
            language,
            code,
            block_index,
            pre_highlighted: None,
        }
    }

    /// Construct with pre-computed highlight styles (from cache).
    pub fn with_highlighted_styles(
        language: Option<String>,
        code: String,
        styles: Vec<(Range<usize>, HighlightStyle)>,
        block_index: usize,
    ) -> Self {
        Self {
            language,
            code,
            block_index,
            pre_highlighted: Some(styles),
        }
    }
}

impl RenderOnce for CodeBlockComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let bg_color = cx.theme().muted;
        let border_color = cx.theme().border;

        let CodeBlockComponent {
            language,
            code,
            block_index,
            pre_highlighted,
        } = self;

        // Use pre-highlighted styles if available, otherwise compute
        let styles = match pre_highlighted {
            Some(s) => s,
            None => syntax_highlighter::highlight_code(&code, language.as_deref(), cx),
        };

        let styled_text = StyledText::new(code.clone()).with_highlights(styles);

        div()
            .relative() // For absolute positioning of copy button
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .mb_3()
            .p_3()
            .child(
                div()
                    .relative()
                    .font_family("monospace")
                    .text_size(px(13.0))
                    .line_height(relative(1.5))
                    .child(styled_text)
                    // Copy button (top-right overlay)
                    .child(
                        div().absolute().top_0().right_0().child(
                            Button::new(ElementId::Name(
                                format!("copy-code-btn-{}", block_index).into(),
                            ))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CustomIcon::Copy))
                            .tooltip("Copy code")
                            .on_click({
                                let code = code.clone();
                                move |_event, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                }
                            }),
                        ),
                    ),
            )
    }
}
