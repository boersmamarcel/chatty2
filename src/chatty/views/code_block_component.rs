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
}

impl CodeBlockComponent {
    pub fn new(language: Option<String>, code: String, block_index: usize) -> Self {
        Self {
            language,
            code,
            block_index,
        }
    }
}

impl RenderOnce for CodeBlockComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let bg_color = cx.theme().muted;
        let border_color = cx.theme().border;

        // Highlight the code using syntect
        let highlighted_spans =
            syntax_highlighter::highlight_code(&self.code, self.language.as_deref(), cx);

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
                    .child(div().flex().flex_col().gap_0().children(
                        // Group spans by lines
                        self.render_lines(highlighted_spans),
                    ))
                    // Copy button (top-right overlay)
                    .child(
                        div().absolute().top_0().right_0().child(
                            Button::new(ElementId::Name(
                                format!("copy-code-btn-{}", self.block_index).into(),
                            ))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CustomIcon::Copy))
                            .tooltip("Copy code")
                            .on_click({
                                let code = self.code.clone();
                                move |_event, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                }
                            }),
                        ),
                    ),
            )
    }
}

impl CodeBlockComponent {
    /// Render highlighted spans as lines
    fn render_lines(&self, spans: Vec<syntax_highlighter::HighlightedSpan>) -> Vec<Div> {
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
}
