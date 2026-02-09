use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable};
use tracing::debug;

use crate::assets::CustomIcon;

// Inline math dimensions
const INLINE_MATH_MAX_HEIGHT: f32 = 32.0;
const INLINE_MATH_MAX_WIDTH: f32 = 200.0;

// Block math dimensions
const BLOCK_MATH_MAX_WIDTH: f32 = 800.0;
const BLOCK_MATH_MAX_HEIGHT: f32 = 400.0;

// Fallback rendering styles
const FALLBACK_PADDING_X: f32 = 2.0;
const FALLBACK_PADDING_Y: f32 = 2.0;
const FALLBACK_BORDER_WIDTH: f32 = 3.0;

/// Component for rendering LaTeX math expressions as SVG
#[derive(IntoElement, Clone)]
pub struct MathComponent {
    content: String,
    is_inline: bool,
    element_id: ElementId,
    cached_svg_path: Option<std::path::PathBuf>,
}

impl MathComponent {
    pub fn new(content: String, is_inline: bool, element_id: ElementId) -> Self {
        Self {
            content,
            is_inline,
            element_id,
            cached_svg_path: None,
        }
    }

    /// Create with a pre-computed SVG path to avoid re-rendering
    pub fn with_svg_path(
        content: String,
        is_inline: bool,
        element_id: ElementId,
        svg_path: std::path::PathBuf,
    ) -> Self {
        Self {
            content,
            is_inline,
            element_id,
            cached_svg_path: Some(svg_path),
        }
    }

    /// Render SVG-only path (no file I/O, pre-styled SVG already exists)
    fn render_svg_only(&self, svg_path: &std::path::Path, cx: &App) -> Stateful<Div> {
        let id = self.element_id.clone();
        let content = self.content.clone();

        if self.is_inline {
            div()
                .id(id.clone())
                .flex()
                .items_center()
                .child(
                    img(svg_path)
                        .max_h(px(INLINE_MATH_MAX_HEIGHT))
                        .max_w(px(INLINE_MATH_MAX_WIDTH))
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .child(self.build_copy_button(&id, &content, cx))
        } else {
            div()
                .id(id.clone())
                .relative()
                .flex()
                .justify_center()
                .my_3()
                .child(
                    img(svg_path)
                        .max_w(px(BLOCK_MATH_MAX_WIDTH))
                        .max_h(px(BLOCK_MATH_MAX_HEIGHT))
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .child(self.build_copy_button(&id, &content, cx)),
                )
        }
    }

    /// Build a copy button for LaTeX content
    fn build_copy_button(&self, id: &ElementId, content: &str, _cx: &App) -> Button {
        let latex_content = content.to_string();
        Button::new(ElementId::Name(format!("copy-math-{}", id.clone()).into()))
            .ghost()
            .xsmall()
            .icon(Icon::new(CustomIcon::Copy))
            .tooltip("Copy LaTeX")
            .on_click(move |_event, _window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(latex_content.clone()));
            })
    }
}

impl RenderOnce for MathComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // If we have a cached styled SVG path, just display it (no file I/O)
        if let Some(svg_path) = &self.cached_svg_path {
            return self.render_svg_only(svg_path, cx);
        }

        // Fallback: show raw LaTeX if no pre-styled SVG available
        debug!(content = %self.content, is_inline = self.is_inline, "No cached SVG, rendering fallback");
        render_fallback_static(
            &self.content,
            self.is_inline,
            &self.element_id,
            cx,
            "No SVG cached",
        )
    }
}

/// Static fallback rendering function when SVG is unavailable
fn render_fallback_static(
    content: &str,
    is_inline: bool,
    element_id: &ElementId,
    cx: &App,
    error_msg: &str,
) -> Stateful<Div> {
    let bg_color = cx.theme().muted;
    let text_color = cx.theme().foreground;
    let border_color = cx.theme().border;
    let error_color = cx.theme().foreground;

    if is_inline {
        div()
            .id(element_id.clone())
            .px(px(FALLBACK_PADDING_X))
            .py(px(FALLBACK_PADDING_Y))
            .mx(px(2.))
            .bg(bg_color)
            .border(px(FALLBACK_BORDER_WIDTH))
            .border_color(border_color)
            .rounded(px(3.))
            .text_color(text_color)
            .font_family("monospace")
            .child(content.to_string())
    } else {
        div()
            .id(element_id.clone())
            .flex()
            .flex_col()
            .my_3()
            .p_4()
            .bg(bg_color)
            .border(px(FALLBACK_BORDER_WIDTH))
            .border_color(border_color)
            .rounded_md()
            .child(
                div()
                    .text_xs()
                    .text_color(error_color)
                    .mb_2()
                    .child(error_msg.to_string()),
            )
            .child(
                div()
                    .text_color(text_color)
                    .font_family("monospace")
                    .text_size(px(14.))
                    .child(content.to_string()),
            )
    }
}
