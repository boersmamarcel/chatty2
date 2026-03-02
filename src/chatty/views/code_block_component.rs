use super::line_splitter::split_spans_into_lines;
use super::syntax_highlighter;
use crate::assets::CustomIcon;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable};

/// A code block component with syntax highlighting and a copy button
#[derive(IntoElement, Clone)]
pub struct CodeBlockComponent {
    language: Option<String>,
    code: String,
    block_index: usize,
    /// Pre-computed highlighted spans. If Some, skip highlight_code() in render.
    pre_highlighted: Option<Vec<syntax_highlighter::HighlightedSpan>>,
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

    /// Construct with pre-computed highlighted spans (from cache).
    pub fn with_highlighted_spans(
        language: Option<String>,
        code: String,
        spans: Vec<syntax_highlighter::HighlightedSpan>,
        block_index: usize,
    ) -> Self {
        Self {
            language,
            code,
            block_index,
            pre_highlighted: Some(spans),
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

        // Use pre-highlighted spans if available, otherwise compute
        let highlighted_spans = match pre_highlighted {
            Some(spans) => spans,
            None => syntax_highlighter::highlight_code(&code, language.as_deref(), cx),
        };

        let rendered_lines = render_highlighted_lines(highlighted_spans);

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
                    // Render code line by line to preserve formatting
                    .child(div().flex().flex_col().gap_0().children(rendered_lines))
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

/// Render highlighted spans as lines
fn render_highlighted_lines(spans: Vec<syntax_highlighter::HighlightedSpan>) -> Vec<Div> {
    split_spans_into_lines(spans)
        .into_iter()
        .map(|line_spans| {
            div().flex().flex_row().children(
                line_spans
                    .into_iter()
                    .map(|ls| div().text_color(ls.color).child(ls.text))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}
