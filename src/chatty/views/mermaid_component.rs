use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable};

use crate::assets::CustomIcon;

// Mermaid diagram dimensions
const MERMAID_MAX_WIDTH: f32 = 800.0;
const MERMAID_MAX_HEIGHT: f32 = 600.0;

/// Component for rendering Mermaid diagrams as SVG
#[derive(IntoElement, Clone)]
pub struct MermaidComponent {
    source: String,
    element_id: ElementId,
    cached_svg_path: Option<std::path::PathBuf>,
}

impl MermaidComponent {
    pub fn new(source: String, element_id: ElementId) -> Self {
        Self {
            source,
            element_id,
            cached_svg_path: None,
        }
    }

    /// Create with a pre-computed SVG path
    pub fn with_svg_path(
        source: String,
        element_id: ElementId,
        svg_path: std::path::PathBuf,
    ) -> Self {
        Self {
            source,
            element_id,
            cached_svg_path: Some(svg_path),
        }
    }

    /// Build a copy button for Mermaid source
    fn build_copy_button(id: &ElementId, source: &str) -> Button {
        let source = source.to_string();
        Button::new(ElementId::Name(
            format!("copy-mermaid-{}", id.clone()).into(),
        ))
        .ghost()
        .xsmall()
        .icon(Icon::new(CustomIcon::Copy))
        .tooltip("Copy Mermaid")
        .on_click(move |_event, _window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(source.clone()));
        })
    }
}

impl RenderOnce for MermaidComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if let Some(svg_path) = &self.cached_svg_path {
            // Render the SVG diagram with a copy button overlay
            div()
                .id(self.element_id.clone())
                .relative()
                .flex()
                .justify_center()
                .my_3()
                .child(
                    img(svg_path.as_path())
                        .max_w(px(MERMAID_MAX_WIDTH))
                        .max_h(px(MERMAID_MAX_HEIGHT))
                        .object_fit(gpui::ObjectFit::Contain),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .child(Self::build_copy_button(&self.element_id, &self.source)),
                )
        } else {
            // Fallback: render raw mermaid code in a styled box
            let bg_color = cx.theme().muted;
            let border_color = cx.theme().border;
            let text_color = cx.theme().foreground;

            div()
                .id(self.element_id.clone())
                .relative()
                .bg(bg_color)
                .border_1()
                .border_color(border_color)
                .rounded_md()
                .mb_3()
                .p_3()
                .child(
                    div()
                        .font_family("monospace")
                        .text_size(px(13.0))
                        .line_height(relative(1.5))
                        .text_color(text_color)
                        .child(self.source.clone()),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .child(Self::build_copy_button(&self.element_id, &self.source)),
                )
        }
    }
}
