use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use tracing::{info, warn};

use crate::chatty::services::MathRendererService;

/// Component for rendering LaTeX math expressions as SVG
#[derive(IntoElement, Clone)]
pub struct MathComponent {
    content: String,
    is_inline: bool,
    element_id: ElementId,
}

impl MathComponent {
    pub fn new(content: String, is_inline: bool, element_id: ElementId) -> Self {
        Self {
            content,
            is_inline,
            element_id,
        }
    }
}

impl RenderOnce for MathComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        info!("MathComponent::render called for: {}", self.content);

        // Get SVG file path from service
        let svg_path_result = if let Some(service) = cx.try_global::<MathRendererService>() {
            service.render_to_svg_file(&self.content, self.is_inline)
        } else {
            warn!("Math renderer service not initialized");
            Err(anyhow::anyhow!("Service not available"))
        };

        match svg_path_result {
            Ok(svg_path) => {
                info!(path = ?svg_path, "Rendering math from SVG file");

                // Render using SVG file path (NOT data URI)
                if self.is_inline {
                    div()
                        .id(self.element_id.clone())
                        .flex()
                        .items_center()
                        .child(
                            img(svg_path)
                                .max_h(px(24.))
                                .object_fit(gpui::ObjectFit::Contain)
                        )
                } else {
                    div()
                        .id(self.element_id.clone())
                        .flex()
                        .justify_center()
                        .my_3()
                        .child(
                            img(svg_path)
                                .max_w(px(700.))
                                .max_h(px(300.))
                                .object_fit(gpui::ObjectFit::Contain)
                        )
                }
            }
            Err(e) => {
                warn!(error = ?e, content = %self.content, "Failed to render math");
                self.render_fallback(cx, &format!("Math render error: {}", e))
            }
        }
    }
}

impl MathComponent {
    /// Fallback rendering when SVG generation fails
    fn render_fallback(&self, cx: &App, error_msg: &str) -> Stateful<Div> {
        let bg_color = cx.theme().muted;
        let text_color = cx.theme().foreground;
        let border_color = cx.theme().border;
        let error_color = cx.theme().foreground;

        if self.is_inline {
            div()
                .id(self.element_id.clone())
                .px_1()
                .py(px(2.))
                .mx(px(2.))
                .bg(bg_color)
                .border_1()
                .border_color(border_color)
                .rounded(px(3.))
                .text_color(text_color)
                .font_family("monospace")
                .child(self.content.clone())
        } else {
            div()
                .id(self.element_id.clone())
                .flex()
                .flex_col()
                .my_3()
                .p_4()
                .bg(bg_color)
                .border_1()
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
                        .child(self.content.clone()),
                )
        }
    }
}
