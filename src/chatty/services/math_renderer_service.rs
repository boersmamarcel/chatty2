use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use gpui::Global;
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, Source, VirtualPath};
use typst::text::{Font, FontBook};
use typst::{Library, LibraryExt, World};
use typst::utils::LazyHash;

impl Global for MathRendererService {}

/// Minimal World implementation for Typst math rendering
struct MathWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main_id: FileId,
    source: Source,
}

impl MathWorld {
    fn new(content: &str) -> Self {
        let library = LazyHash::new(Library::builder().build());

        // Use Typst's embedded fonts
        let fonts = typst_assets::fonts().map(|data| {
            Font::new(Bytes::new(data), 0).unwrap()
        }).collect::<Vec<_>>();

        let book = LazyHash::new(FontBook::from_fonts(fonts.iter()));

        // Create virtual file ID for the main file
        let main_id = FileId::new(None, VirtualPath::new("main.typ"));

        // Create source
        let source = Source::new(main_id, content.to_string());

        Self {
            library,
            book,
            fonts,
            main_id,
            source,
        }
    }
}

impl World for MathWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main_id
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main_id {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound(id.vpath().as_rootless_path().into()))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(id.vpath().as_rootless_path().into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<i64>) -> Option<Datetime> {
        Some(Datetime::from_ymd(2024, 1, 1).unwrap())
    }
}

/// Math renderer service that converts LaTeX to SVG using Typst
pub struct MathRendererService {
    cache: Arc<Mutex<HashMap<String, String>>>,
}

impl MathRendererService {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Render LaTeX math expression to SVG
    pub fn render_to_svg(&self, latex: &str, is_inline: bool) -> Result<String> {
        // Create cache key from content + type
        let cache_key = self.make_cache_key(latex, is_inline);

        // Check cache first
        if let Ok(cache) = self.cache.lock() {
            if let Some(svg) = cache.get(&cache_key) {
                debug!(latex, "Math cache hit");
                return Ok(svg.clone());
            }
        }

        debug!(latex, is_inline, "Rendering math to SVG");

        // Convert LaTeX to Typst using MiTeX
        let mut typst_code = mitex::convert_math(latex, None)
            .map_err(|e| anyhow::anyhow!("Failed to convert LaTeX to Typst: {} - {}", latex, e))?;

        // Fix MiTeX bugs - MiTeX generates invalid Typst functions
        typst_code = typst_code.replace("mitexsqrt", "sqrt");
        typst_code = typst_code.replace("mitexmathbf", "bold");
        typst_code = typst_code.replace("tfrac", "frac");
        typst_code = typst_code.replace("pmatrix", "mat");
        typst_code = typst_code.replace("aligned", "cases");  // Approximation for aligned environments
        
        // Fix textmath - MiTeX wraps text in #textmath[...] but Typst doesn't have textmath
        // Replace with proper text function or quoted strings
        while let Some(start) = typst_code.find("#textmath[") {
            let after_bracket = start + 10; // length of "#textmath["
            if let Some(end) = typst_code[after_bracket..].find(']') {
                let text_content = &typst_code[after_bracket..after_bracket + end];
                // Replace #textmath[content] with #text[content]
                typst_code = format!(
                    "{}#text[{}]{}",
                    &typst_code[..start],
                    text_content,
                    &typst_code[after_bracket + end + 1..]
                );
            } else {
                break; // No closing bracket found
            }
        }

        info!(typst_code = %typst_code, "MiTeX converted LaTeX to Typst (after fixes)");

        // Wrap in Typst document template with minimal page size
        let doc_content = if is_inline {
            format!("#set page(width: auto, height: auto, margin: (x: 4pt, y: 6pt))
${typst_code}$")
        } else {
            // Spaces around content make it display math
            format!("#set page(width: auto, height: auto, margin: (x: 8pt, y: 10pt))
$ {typst_code} $")
        };

        // Compile with Typst
        let svg = self
            .compile_typst_to_svg(&doc_content)
            .context("Failed to compile Typst to SVG")?;

        // Store in cache
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key, svg.clone());
        }

        Ok(svg)
    }

    /// Render LaTeX to SVG and write to persistent cache file
    ///
    /// This method generates an SVG file from LaTeX math and stores it in a persistent
    /// cache directory (~/.config/chatty/math_cache/). The cache survives app restarts
    /// and allows GPUI to load the SVG images as file paths (which GPUI requires).
    ///
    /// Returns the PathBuf to the cached SVG file.
    pub fn render_to_svg_file(&self, latex: &str, is_inline: bool) -> Result<std::path::PathBuf> {
        // Get or generate SVG (uses existing in-memory cache)
        let svg_data = self.render_to_svg(latex, is_inline)?;
        
        // Create persistent cache directory
        let cache_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("No config directory"))?
            .join("chatty")
            .join("math_cache");
        
        std::fs::create_dir_all(&cache_dir)
            .context("Failed to create math cache directory")?;
        
        // Use hash as filename for deterministic caching
        let cache_key = self.make_cache_key(latex, is_inline);
        let svg_path = cache_dir.join(format!("{}.svg", cache_key));
        
        // Only write if file doesn't exist (cache hit)
        if !svg_path.exists() {
            // Strip width/height attributes from SVG to allow GPUI to scale it
            // Typst generates SVGs with small pt dimensions that GPUI respects literally
            let svg_without_dims = self.strip_svg_dimensions(&svg_data);
            
            std::fs::write(&svg_path, svg_without_dims)
                .context("Failed to write SVG to cache")?;
            info!(path = ?svg_path, "Wrote math SVG to persistent cache");
        } else {
            debug!(path = ?svg_path, "Math SVG cache hit");
        }
        
        Ok(svg_path)
    }

    fn make_cache_key(&self, latex: &str, is_inline: bool) -> String {
        let mut hasher = Sha256::new();
        hasher.update(latex.as_bytes());
        hasher.update(if is_inline { b"inline" } else { b"block " });
        format!("{:x}", hasher.finalize())
    }

    /// Compile Typst source to SVG
    fn compile_typst_to_svg(&self, typst_content: &str) -> Result<String> {
        // Create a minimal World for this compilation
        let world = MathWorld::new(typst_content);

        // Compile the document
        let warned_result = typst::compile::<typst::layout::PagedDocument>(&world);

        // Extract the document, handling any errors
        let document = warned_result.output.map_err(|errors| {
            let error_messages: Vec<String> = errors
                .iter()
                .map(|e| format!("{}", e.message))
                .collect();
            anyhow::anyhow!("Typst compilation failed: {}", error_messages.join(", "))
        })?;

        // Render the first (and only) page to SVG
        let svg_data = typst_svg::svg_frame(&document.pages[0].frame);

        Ok(svg_data)
    }

    /// Clear the math rendering cache
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// Strip width and height attributes from SVG and add proper scaling
    fn strip_svg_dimensions(&self, svg: &str) -> String {
        // Remove width="..." and height="..." attributes
        // Keep viewBox as it's needed for aspect ratio
        let mut result = svg.to_string();
        
        // Find and remove width attribute
        if let Some(width_start) = result.find(r#" width=""#) {
            if let Some(width_end) = result[width_start..].find('"') {
                // Find the closing quote
                let quote_pos = width_start + width_end + 1;
                if let Some(closing_quote) = result[quote_pos..].find('"') {
                    result.replace_range(width_start..quote_pos + closing_quote + 1, "");
                }
            }
        }
        
        // Find and remove height attribute
        if let Some(height_start) = result.find(r#" height=""#) {
            if let Some(height_end) = result[height_start..].find('"') {
                // Find the closing quote
                let quote_pos = height_start + height_end + 1;
                if let Some(closing_quote) = result[quote_pos..].find('"') {
                    result.replace_range(height_start..quote_pos + closing_quote + 1, "");
                }
            }
        }
        
        // Add width/height based on viewBox but scaled up 2x for better visibility
        // Extract viewBox to calculate dimensions
        if let Some(viewbox_start) = result.find(r#"viewBox=""#) {
            let vb_start = viewbox_start + 9; // length of 'viewBox="'
            if let Some(vb_end) = result[vb_start..].find('"') {
                let viewbox = &result[vb_start..vb_start + vb_end];
                let parts: Vec<&str> = viewbox.split_whitespace().collect();
                if parts.len() == 4 {
                    // viewBox format: "minX minY width height"
                    if let (Ok(width), Ok(height)) = (parts[2].parse::<f64>(), parts[3].parse::<f64>()) {
                        // Scale by 1.5x for better readability (matches typical font sizes)
                        let scaled_width = width * 1.5;
                        let scaled_height = height * 1.5;
                        
                        // Insert width and height attributes after viewBox
                        let insert_pos = vb_start + vb_end + 1; // after closing quote of viewBox
                        let size_attrs = format!(r#" width="{}pt" height="{}pt""#, scaled_width, scaled_height);
                        result.insert_str(insert_pos, &size_attrs);
                    }
                }
            }
        }
        
        result
    }

    /// Get the number of cached items
    pub fn cache_size(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}
