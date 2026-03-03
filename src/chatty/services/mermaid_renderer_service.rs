use anyhow::{Context, Result};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

use gpui::Global;

impl Global for MermaidRendererService {}

/// Mermaid diagram renderer service that converts Mermaid syntax to SVG
///
/// Uses `mermaid-rs-renderer` for pure-Rust rendering (no browser needed).
/// Supports 23 diagram types including flowchart, sequence, class, state, etc.
///
/// Caching: in-memory HashMap + disk cache at `{config_dir}/chatty/mermaid_cache/`.
/// Dark/light mode variants are cached separately via the `is_dark` hash flag.
pub struct MermaidRendererService {
    cache: Arc<Mutex<HashMap<String, String>>>,
}

impl MermaidRendererService {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Render Mermaid source to SVG string
    pub fn render_to_svg(&self, source: &str, is_dark: bool) -> Result<String> {
        let cache_key = self.make_cache_key(source, is_dark);

        // Check in-memory cache
        if let Ok(cache) = self.cache.lock()
            && let Some(svg) = cache.get(&cache_key)
        {
            debug!("Mermaid cache hit");
            return Ok(svg.clone());
        }

        debug!(is_dark, "Rendering mermaid diagram to SVG");

        let mut opts = if is_dark {
            mermaid_rs_renderer::RenderOptions {
                theme: Self::dark_theme(),
                ..Default::default()
            }
        } else {
            mermaid_rs_renderer::RenderOptions::default()
        };

        // Work around mermaid-rs-renderer not XML-escaping font_family in
        // <text> attributes. The default modern theme contains `"Segoe UI"`
        // which produces invalid XML: font-family="..., "Segoe UI", ...".
        // Use a quote-free font stack to avoid the parse failure in usvg.
        opts.theme.font_family =
            "Inter, ui-sans-serif, system-ui, -apple-system, sans-serif".to_string();

        let svg = mermaid_rs_renderer::render_with_options(source, opts)
            .map_err(|e| anyhow::anyhow!("Failed to render mermaid diagram: {}", e))?;

        // Store in cache
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key, svg.clone());
        }

        Ok(svg)
    }

    /// Render Mermaid to SVG and write to persistent cache file
    ///
    /// Returns the PathBuf to the cached SVG file.
    pub fn render_to_svg_file(&self, source: &str, is_dark: bool) -> Result<std::path::PathBuf> {
        let svg_data = self.render_to_svg(source, is_dark)?;

        let cache_dir = Self::cache_dir()?;
        std::fs::create_dir_all(&cache_dir).context("Failed to create mermaid cache directory")?;

        let cache_key = self.make_cache_key(source, is_dark);
        let svg_path = cache_dir.join(format!("{}.svg", cache_key));

        if !svg_path.exists() {
            let sanitized = Self::sanitize_svg(&svg_data);

            // Validate XML before writing to catch upstream rendering bugs
            if sanitized.contains("\"Segoe") || sanitized.contains("\"segoe") {
                warn!("Mermaid SVG still contains unescaped Segoe UI quotes after fix");
            }

            std::fs::write(&svg_path, &sanitized).context("Failed to write SVG to cache")?;
            info!(path = ?svg_path, len = sanitized.len(), "Wrote mermaid SVG to persistent cache");
        } else {
            debug!(path = ?svg_path, "Mermaid SVG cache hit");
        }

        Ok(svg_path)
    }

    /// Build a dark theme for mermaid rendering
    fn dark_theme() -> mermaid_rs_renderer::Theme {
        let mut theme = mermaid_rs_renderer::Theme::modern();
        theme.primary_color = "#313244".to_string();
        theme.primary_text_color = "#cdd6f4".to_string();
        theme.primary_border_color = "#585b70".to_string();
        theme.line_color = "#a6adc8".to_string();
        theme.secondary_color = "#45475a".to_string();
        theme.tertiary_color = "#585b70".to_string();
        theme.background = "#1e1e2e".to_string();
        theme.edge_label_background = "#313244".to_string();
        theme.cluster_background = "#313244".to_string();
        theme.cluster_border = "#585b70".to_string();
        theme
    }

    /// Sanitize SVG for usvg/resvg compatibility.
    ///
    /// mermaid-rs-renderer emits SVG with `class`, `data-*`, and `style`
    /// attributes plus `<style>` blocks that usvg (GPUI's SVG parser) rejects.
    /// This strips them while preserving marker `id` attributes needed for
    /// arrowhead references (`url(#marker-id)`).
    fn sanitize_svg(svg: &str) -> String {
        // Remove <style>...</style> blocks entirely
        let style_re = Regex::new(r"<style[^>]*>[\s\S]*?</style>").unwrap();
        let result = style_re.replace_all(svg, "");

        // Remove class="..." attributes
        let class_re = Regex::new(r#"\s+class="[^"]*""#).unwrap();
        let result = class_re.replace_all(&result, "");

        // Remove data-*="..." attributes
        let data_re = Regex::new(r#"\s+data-[a-z\-]+="[^"]*""#).unwrap();
        let result = data_re.replace_all(&result, "");

        // Remove style="mix-blend-mode: multiply;" (unsupported by resvg)
        let blend_re = Regex::new(r#"\s+style="[^"]*mix-blend-mode[^"]*""#).unwrap();
        let result = blend_re.replace_all(&result, "");

        result.into_owned()
    }

    /// Render a cached SVG file to PNG bytes at 2x scale for crisp output.
    ///
    /// Uses resvg (same renderer GPUI uses) so the output matches what the user sees.
    /// Loads system fonts so that text elements render correctly.
    pub fn render_svg_to_png(svg_path: &std::path::Path) -> Result<Vec<u8>> {
        use std::sync::{Arc, LazyLock};

        // Lazily load system fonts once (same pattern as GPUI's SvgRenderer)
        static FONT_DB: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        });

        let svg_data = std::fs::read(svg_path).context("Failed to read SVG file")?;

        let default_font_resolver = usvg::FontResolver::default_font_selector();
        let font_resolver = Box::new(
            move |font: &usvg::Font, db: &mut Arc<usvg::fontdb::Database>| {
                if db.is_empty() {
                    *db = FONT_DB.clone();
                }
                default_font_resolver(font, db)
            },
        );
        let opts = usvg::Options {
            font_resolver: usvg::FontResolver {
                select_font: font_resolver,
                select_fallback: usvg::FontResolver::default_fallback_selector(),
            },
            ..Default::default()
        };

        let tree = usvg::Tree::from_data(&svg_data, &opts)
            .map_err(|e| anyhow::anyhow!("Failed to parse SVG: {}", e))?;

        let svg_size = tree.size();
        let scale = 2.0_f32; // 2x for crisp output

        let width = (svg_size.width() * scale) as u32;
        let height = (svg_size.height() * scale) as u32;

        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| anyhow::anyhow!("Failed to create pixmap ({}x{})", width, height))?;

        // Fill with white background
        pixmap.fill(resvg::tiny_skia::Color::WHITE);

        let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        pixmap
            .encode_png()
            .map_err(|e| anyhow::anyhow!("Failed to encode PNG: {}", e))
    }

    /// Cache version — bump whenever rendering or sanitization logic changes
    /// to invalidate stale on-disk SVGs from previous builds.
    const CACHE_VERSION: &'static str = "v3";

    fn make_cache_key(&self, source: &str, is_dark: bool) -> String {
        let mut hasher = Sha256::new();
        hasher.update(Self::CACHE_VERSION.as_bytes());
        hasher.update(source.as_bytes());
        hasher.update(if is_dark { "dark" } else { "light" }.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Get the cache directory path
    fn cache_dir() -> Result<std::path::PathBuf> {
        let cache_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("No config directory"))?
            .join("chatty")
            .join("mermaid_cache");
        Ok(cache_dir)
    }

    /// Cleans up old cached SVGs from previous sessions
    pub fn cleanup_old_svgs() -> Result<()> {
        let cache_dir = Self::cache_dir()?;

        if !cache_dir.exists() {
            return Ok(());
        }

        let mut removed_count = 0;

        for entry in std::fs::read_dir(&cache_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_name().and_then(|f| f.to_str())
                && filename.ends_with(".svg")
            {
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!(path = ?path, error = ?e, "Failed to remove cached mermaid SVG");
                } else {
                    removed_count += 1;
                }
            }
        }

        if removed_count > 0 {
            info!(count = removed_count, "Cleaned up old mermaid SVGs");
        }

        Ok(())
    }

    /// Clear the rendering cache
    #[allow(dead_code)]
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// Get the number of cached items
    #[allow(dead_code)]
    pub fn cache_size(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_starts_empty() {
        let service = MermaidRendererService::new();
        assert_eq!(service.cache_size(), 0);
    }

    #[test]
    fn test_cache_populates_after_render() {
        let service = MermaidRendererService::new();
        service.render_to_svg("flowchart LR; A-->B", false).unwrap();
        assert_eq!(service.cache_size(), 1);
    }

    #[test]
    fn test_cache_deduplicates_same_input() {
        let service = MermaidRendererService::new();
        service.render_to_svg("flowchart LR; A-->B", false).unwrap();
        service.render_to_svg("flowchart LR; A-->B", false).unwrap();
        assert_eq!(service.cache_size(), 1);
    }

    #[test]
    fn test_cache_distinguishes_dark_vs_light() {
        let service = MermaidRendererService::new();
        service.render_to_svg("flowchart LR; A-->B", false).unwrap();
        service.render_to_svg("flowchart LR; A-->B", true).unwrap();
        assert_eq!(service.cache_size(), 2);
    }

    #[test]
    fn test_clear_cache() {
        let service = MermaidRendererService::new();
        service.render_to_svg("flowchart LR; A-->B", false).unwrap();
        service.render_to_svg("flowchart TD; X-->Y", true).unwrap();
        assert_eq!(service.cache_size(), 2);

        service.clear_cache();
        assert_eq!(service.cache_size(), 0);
    }

    #[test]
    fn test_render_simple_flowchart() {
        let service = MermaidRendererService::new();
        let svg = service
            .render_to_svg("flowchart LR; A-->B-->C", false)
            .unwrap();
        assert!(svg.contains("<svg"), "Output should be SVG");
    }

    #[test]
    fn test_sanitize_strips_class_and_data_attrs() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><g class="nodes" data-edge-id="e1"><rect class="node" data-label-kind="center" width="10"/></g></svg>"#;
        let sanitized = MermaidRendererService::sanitize_svg(svg);
        assert!(!sanitized.contains("class="));
        assert!(!sanitized.contains("data-"));
        assert!(sanitized.contains("<rect"));
        assert!(sanitized.contains("<svg"));
    }

    #[test]
    fn test_sanitize_preserves_marker_ids() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><defs><marker id="arrow-1" viewBox="0 0 10 10"><path d="M 0 0 L 10 5 L 0 10 z"/></marker></defs><path marker-end="url(#arrow-1)"/></svg>"#;
        let sanitized = MermaidRendererService::sanitize_svg(svg);
        assert!(
            sanitized.contains(r#"id="arrow-1""#),
            "Marker id should be preserved for url(#...) references"
        );
    }

    #[test]
    fn test_sanitize_strips_style_blocks() {
        let svg = r#"<svg><style>svg{font-family:sans;}</style><rect width="10"/></svg>"#;
        let sanitized = MermaidRendererService::sanitize_svg(svg);
        assert!(!sanitized.contains("<style"));
        assert!(!sanitized.contains("</style>"));
        assert!(sanitized.contains("<rect"));
    }

    #[test]
    fn test_render_invalid_syntax_returns_error() {
        let service = MermaidRendererService::new();
        let result = service.render_to_svg("this is not valid mermaid at all!!!", false);
        assert!(result.is_err(), "Invalid mermaid should return error");
    }

    #[test]
    fn test_sanitized_svg_has_no_segoe_quotes() {
        let service = MermaidRendererService::new();
        let svg = service
            .render_to_svg("flowchart LR; A-->B-->C", false)
            .unwrap();
        let sanitized = MermaidRendererService::sanitize_svg(&svg);
        assert!(
            !sanitized.contains("\"Segoe"),
            "Sanitized SVG must not contain unescaped Segoe UI quotes"
        );
        assert!(
            !sanitized.contains("Segoe"),
            "Font-family override should remove Segoe UI entirely"
        );
    }
}
