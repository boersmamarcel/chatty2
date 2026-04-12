use super::syntax_highlighter;
use crate::assets::CustomIcon;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable};
use std::ops::Range;

const LANGUAGE_BADGE_BG_OPACITY: f32 = 0.35;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodeBlockRenderMode {
    Highlighted,
    Plain,
}

/// A code block component with syntax highlighting and a copy button
#[derive(IntoElement, Clone)]
pub struct CodeBlockComponent {
    language: Option<String>,
    code: String,
    block_index: usize,
    /// Pre-computed highlight styles. If Some, skip highlight_code() in render.
    pre_highlighted: Option<Vec<(Range<usize>, HighlightStyle)>>,
    render_mode: CodeBlockRenderMode,
}

impl CodeBlockComponent {
    #[allow(dead_code)]
    pub fn new(language: Option<String>, code: String, block_index: usize) -> Self {
        Self {
            language,
            code,
            block_index,
            pre_highlighted: None,
            render_mode: CodeBlockRenderMode::Highlighted,
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
            render_mode: CodeBlockRenderMode::Highlighted,
        }
    }

    pub fn plain(language: Option<String>, code: String, block_index: usize) -> Self {
        Self {
            language,
            code,
            block_index,
            pre_highlighted: Some(vec![]),
            render_mode: CodeBlockRenderMode::Plain,
        }
    }
}

impl RenderOnce for CodeBlockComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        let CodeBlockComponent {
            language,
            code,
            block_index,
            pre_highlighted,
            render_mode,
        } = self;

        // Use pre-highlighted styles if available, otherwise compute
        let styles = match pre_highlighted {
            Some(s) => s,
            None if render_mode == CodeBlockRenderMode::Highlighted => {
                syntax_highlighter::highlight_code(&code, language.as_deref(), cx)
            }
            None => vec![],
        };

        let styled_text = StyledText::new(code.clone()).with_highlights(styles);
        let bg_color = theme.muted;
        let border_color = theme.border;
        let header_text_color = theme.muted_foreground;
        div()
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .mb_3()
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .mb_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(language.clone().map(|lang| {
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(border_color.opacity(LANGUAGE_BADGE_BG_OPACITY))
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(header_text_color)
                                    .child(lang)
                                    .into_any_element()
                            })),
                    )
                    .child(
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
            )
            .child(
                div()
                    .font_family("monospace")
                    .text_size(px(13.0))
                    .line_height(relative(1.5))
                    .text_color(theme.foreground)
                    .child(styled_text),
            )
    }
}
