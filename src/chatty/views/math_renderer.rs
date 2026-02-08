use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::{Icon, Sizable};
use gpui_component::button::{Button, ButtonVariants};
use tracing::{debug, warn};

use crate::chatty::services::MathRendererService;
use crate::assets::CustomIcon;


/// Inject CSS color styling into SVG content
fn inject_svg_color(svg_content: &str, color: gpui::Hsla) -> String {
    // Convert GPUI Hsla to hex color
    let rgb = color.to_rgb();
    let hex_color = format!(
        "#{:02x}{:02x}{:02x}",
        (rgb.r * 255.0) as u8,
        (rgb.g * 255.0) as u8,
        (rgb.b * 255.0) as u8
    );
    
    // Find the opening <svg tag and inject a <style> element
    // Note: Typst renders math as <path> elements (vector graphics), not <text> elements
    if let Some(svg_pos) = svg_content.find("<svg") {
        if let Some(tag_end) = svg_content[svg_pos..].find('>') {
            let insert_pos = svg_pos + tag_end + 1;
            let style_tag = format!(
                r#"<style>path {{ fill: {} !important; }} text, tspan {{ fill: {} !important; }}</style>"#,
                hex_color, hex_color
            );
            
            let mut result = String::with_capacity(svg_content.len() + style_tag.len());
            result.push_str(&svg_content[..insert_pos]);
            result.push_str(&style_tag);
            result.push_str(&svg_content[insert_pos..]);
            return result;
        }
    }
    
    // If we couldn't inject the style, return original
    svg_content.to_string()
}

/// Component for rendering LaTeX math expressions as SVG
#[derive(IntoElement, Clone)]
pub struct MathComponent {
    content: String,
    is_inline: bool,
    element_id: ElementId,
    cached_svg_path: Option<std::path::PathBuf>,
}

/// Static fallback rendering function
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
            .px_1()
            .py(px(2.))
            .mx(px(2.))
            .bg(bg_color)
            .border_1()
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
                    .child(content.to_string()),
            )
    }
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
    pub fn with_svg_path(content: String, is_inline: bool, element_id: ElementId, svg_path: std::path::PathBuf) -> Self {
        Self {
            content,
            is_inline,
            element_id,
            cached_svg_path: Some(svg_path),
        }
    }
}

impl RenderOnce for MathComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Clone content for error handling since self will be moved
        let content_clone = self.content.clone();
        let is_inline = self.is_inline;
        let element_id = self.element_id.clone();
        
        // Use cached path if available, otherwise generate it
        let svg_path_result = if let Some(cached_path) = self.cached_svg_path {
            Ok(cached_path)
        } else {
            // Get SVG file path from service
            if let Some(service) = cx.try_global::<MathRendererService>() {
                service.render_to_svg_file(&content_clone, is_inline)
            } else {
                warn!("Math renderer service not initialized");
                Err(anyhow::anyhow!("Service not available"))
            }
        };

        match svg_path_result {
            Ok(svg_path) => {
                // Read SVG file and inject theme-aware CSS styling
                let text_color = cx.theme().foreground;
                
                match std::fs::read_to_string(&svg_path) {
                    Ok(svg_content) => {
                        // Inject CSS color styling
                        let styled_svg = inject_svg_color(&svg_content, text_color);
                        
                        // Write styled SVG to a theme-specific file (GPUI requires file paths)
                        // Include theme color in filename so it updates when theme changes
                        let color_hash = format!("{:02x}{:02x}{:02x}",
                            (text_color.to_rgb().r * 255.0) as u8,
                            (text_color.to_rgb().g * 255.0) as u8,
                            (text_color.to_rgb().b * 255.0) as u8
                        );
                        let temp_path = svg_path.with_extension(format!("styled.{}.svg", color_hash));
                        match std::fs::write(&temp_path, styled_svg) {
                            Ok(_) => {
                                // Render using styled SVG
                                if is_inline {
                                    div()
                                        .id(element_id.clone())
                                        .flex()
                                        .items_center()
                                        .child(
                                            img(temp_path)
                                                .max_h(px(32.))
                                                .max_w(px(200.))
                                                .object_fit(gpui::ObjectFit::Contain)
                                        )
                                } else {
                                    div()
                                        .id(element_id.clone())
                                        .relative()
                                        .flex()
                                        .justify_center()
                                        .my_3()
                                        .child(
                                            img(temp_path)
                                                .max_w(px(800.))
                                                .max_h(px(400.))
                                                .object_fit(gpui::ObjectFit::Contain)
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .right_0()
                                                .child(
                                                    Button::new(ElementId::Name(
                                                        format!("copy-math-{}", element_id.clone()).into(),
                                                    ))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(Icon::new(CustomIcon::Copy))
                                                    .tooltip("Copy LaTeX")
                                                    .on_click({
                                                        let latex = content_clone.clone();
                                                        move |_event, _window, cx| {
                                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                                latex.clone(),
                                                            ));
                                                        }
                                                    })
                                                )
                                        )
                                }
                            }
                            Err(e) => {
                                warn!(error = ?e, "Failed to write styled SVG, using original");
                                // Fallback to original SVG
                                if is_inline {
                                    div()
                                        .id(element_id.clone())
                                        .flex()
                                        .items_center()
                                        .child(
                                            img(svg_path)
                                                .max_h(px(32.))
                                                .max_w(px(200.))
                                                .object_fit(gpui::ObjectFit::Contain)
                                        )
                                } else {
                                    div()
                                        .id(element_id.clone())
                                        .relative()
                                        .flex()
                                        .justify_center()
                                        .my_3()
                                        .child(
                                            img(svg_path)
                                                .max_w(px(800.))
                                                .max_h(px(400.))
                                                .object_fit(gpui::ObjectFit::Contain)
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .right_0()
                                                .child(
                                                    Button::new(ElementId::Name(
                                                        format!("copy-math-fallback-{}", element_id.clone()).into(),
                                                    ))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(Icon::new(CustomIcon::Copy))
                                                    .tooltip("Copy LaTeX")
                                                    .on_click({
                                                        let latex = content_clone.clone();
                                                        move |_event, _window, cx| {
                                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                                latex.clone(),
                                                            ));
                                                        }
                                                    })
                                                )
                                        )
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "Failed to read SVG file, using original");
                        // Fallback to original SVG
                        if is_inline {
                            div()
                                .id(element_id.clone())
                                .flex()
                                .items_center()
                                .child(
                                    img(svg_path)
                                        .max_h(px(32.))
                                        .max_w(px(200.))
                                        .object_fit(gpui::ObjectFit::Contain)
                                )
                        } else {
                            div()
                                .id(element_id.clone())
                                .relative()
                                .flex()
                                .justify_center()
                                .my_3()
                                .child(
                                    img(svg_path)
                                        .max_w(px(800.))
                                        .max_h(px(400.))
                                        .object_fit(gpui::ObjectFit::Contain)
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .child(
                                            Button::new(ElementId::Name(
                                                format!("copy-math-original-{}", element_id.clone()).into(),
                                            ))
                                            .ghost()
                                            .xsmall()
                                            .icon(Icon::new(CustomIcon::Copy))
                                            .tooltip("Copy LaTeX")
                                            .on_click({
                                                let latex = content_clone.clone();
                                                move |_event, _window, cx| {
                                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                                        latex.clone(),
                                                    ));
                                                }
                                            })
                                        )
                                )
                        }
                    }
                }
            }
            Err(e) => {
                debug!(error = ?e, content = %content_clone, "Failed to render math");
                render_fallback_static(&content_clone, is_inline, &element_id, cx, &format!("Math render error: {}", e))
            }
        }
    }
}

/// Static fallback rendering function
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
