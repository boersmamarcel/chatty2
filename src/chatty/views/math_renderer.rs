use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use tracing::{info, warn};
use sha2::{Sha256, Digest};
use resvg::usvg;
use resvg::tiny_skia;

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

        // Try to get the math renderer service
        let svg_result = if let Some(service) = cx.try_global::<MathRendererService>() {
            service.render_to_svg(&self.content, self.is_inline)
        } else {
            warn!("Math renderer service not initialized");
            Err(anyhow::anyhow!("Service not available"))
        };

        match svg_result {
            Ok(svg_data) => {
                info!(content = %self.content, "Rendered math to SVG");

                // Use SHA-256 hash for stable, unique filenames
                let mut hasher = Sha256::new();
                hasher.update(&self.content);
                hasher.update(if self.is_inline { b"inline" } else { b"block " });
                let hash = format!("{:x}", hasher.finalize());
                let filename = format!("chatty_math_{}.png", &hash[..16]);

                // Use same temp directory as PDF thumbnails
                use crate::chatty::services::pdf_thumbnail::get_thumbnail_dir;
                
                let cache_dir = match get_thumbnail_dir() {
                    Ok(dir) => dir,
                    Err(e) => {
                        warn!(error = ?e, "Failed to get thumbnail directory");
                        return self.render_fallback(cx, "Failed to get temp directory");
                    }
                };

                let png_path = cache_dir.join(&filename);

                // Check if already cached
                if !png_path.exists() {
                    info!("Math cache miss, rendering: {:?}", png_path);
                    
                    // Convert SVG to PNG
                    if let Err(e) = svg_to_png(&svg_data, &png_path) {
                        warn!(error = ?e, "Failed to convert SVG to PNG");
                        return self.render_fallback(cx, &format!("Failed to convert SVG: {}", e));
                    }
                    
                    info!("Wrote math PNG to file: {:?}", png_path);
                } else {
                    info!("Math cache hit: {:?}", png_path);
                }

                // Match render_file_chip pattern exactly
                // Canonicalize path to ensure it's absolute
                let png_path_opt = png_path.canonicalize().ok();
                
                if png_path_opt.is_none() {
                    warn!("Failed to canonicalize path: {:?}", png_path);
                    return self.render_fallback(cx, "Failed to resolve image path");
                }

                if self.is_inline {
                    div()
                        .id(self.element_id.clone())
                        .flex()
                        .items_center()
                        .when_some(png_path_opt.clone(), |div, img_path| {
                            info!("Inline math: passing path to img(): {:?}", img_path);
                            div.child(
                                img(img_path)
                                    .max_w(px(200.))
                                    .max_h(px(40.))
                            )
                        })
                } else {
                    div()
                        .id(self.element_id.clone())
                        .flex()
                        .justify_center()
                        .my_3()
                        .when_some(png_path_opt, |div, img_path| {
                            info!("Block math: passing path to img(): {:?}", img_path);
                            div.child(
                                img(img_path)
                                    .max_w(px(600.))
                                    .max_h(px(300.))
                            )
                        })
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


/// Convert SVG data to PNG file
fn svg_to_png(svg_data: &str, output_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    // Parse SVG
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg_data, &opts)?;

    // Get size
    let size = tree.size();
    let width = size.width() as u32;
    let height = size.height() as u32;

    // Create pixmap
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or("Failed to create pixmap")?;

    // Render
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    // Save as PNG
    pixmap.save_png(output_path)?;

    Ok(())
}
