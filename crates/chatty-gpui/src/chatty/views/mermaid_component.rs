use gpui::prelude::*;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, Sizable};
use tracing::warn;

use crate::assets::CustomIcon;
use crate::chatty::services::MermaidRendererService;

// Mermaid diagram dimensions
const MERMAID_MAX_WIDTH: f32 = 800.0;
const MERMAID_MAX_HEIGHT: f32 = 600.0;

/// Copy raw PNG bytes to the Linux system clipboard.
///
/// GPUI's Linux `write_to_clipboard` silently discards image entries (it only
/// calls `set_text` under the hood). Instead we pipe the bytes to an external
/// clipboard tool that serves requests in the background:
///
/// - `wl-copy --type image/png` on Wayland (from `wl-clipboard` package)
/// - `xclip -selection clipboard -t image/png` on X11 (from `xclip` package)
///
/// Both tools run as background processes, keeping clipboard ownership until
/// another application reads the data. Returns `true` if a tool was found and
/// launched successfully.
#[cfg(target_os = "linux")]
pub(super) fn copy_png_to_linux_clipboard(png_bytes: &[u8]) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Try wl-copy first (Wayland)
    if let Ok(mut child) = Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(png_bytes);
            // Dropping stdin sends EOF; wl-copy stays alive serving the clipboard.
        }
        return true;
    }

    // Fall back to xclip (X11)
    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "image/png"])
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(png_bytes);
            // xclip stays alive in the background serving clipboard requests.
        }
        return true;
    }

    false
}

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

    /// Build a copy-source button for Mermaid code
    fn build_copy_source_button(id: &ElementId, source: &str) -> Button {
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

    /// Build a copy-as-PNG button for the rendered diagram
    fn build_copy_png_button(id: &ElementId, svg_path: &std::path::Path) -> Button {
        let svg_path = svg_path.to_path_buf();
        Button::new(ElementId::Name(format!("copy-png-{}", id.clone()).into()))
            .ghost()
            .xsmall()
            .icon(Icon::new(CustomIcon::Image))
            .tooltip("Copy as PNG")
            .on_click(move |_event, _window, _cx| {
                match MermaidRendererService::render_svg_to_png(&svg_path) {
                    Ok(png_bytes) => {
                        #[cfg(target_os = "linux")]
                        {
                            // GPUI's Linux clipboard silently discards image entries —
                            // write_to_clipboard only calls set_text() and ignores image bytes.
                            // Use wl-copy (Wayland) or xclip (X11) directly instead.
                            if !copy_png_to_linux_clipboard(&png_bytes) {
                                warn!("No clipboard tool found (install wl-clipboard or xclip)");
                            }
                        }
                        #[cfg(not(target_os = "linux"))]
                        {
                            let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, png_bytes);
                            _cx.write_to_clipboard(ClipboardItem::new_image(&image));
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "Failed to render mermaid PNG for clipboard");
                    }
                }
            })
    }
}

impl RenderOnce for MermaidComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if let Some(svg_path) = &self.cached_svg_path {
            let copy_png = Self::build_copy_png_button(&self.element_id, svg_path);

            // Render the SVG diagram with copy buttons overlay
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
                        .flex()
                        .gap_0p5()
                        .child(copy_png)
                        .child(Self::build_copy_source_button(
                            &self.element_id,
                            &self.source,
                        )),
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
                        .child(Self::build_copy_source_button(
                            &self.element_id,
                            &self.source,
                        )),
                )
        }
    }
}
