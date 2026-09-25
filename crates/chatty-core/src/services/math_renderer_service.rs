use anyhow::{Context, Result};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use tracing::{debug, info, warn};

// Pre-compiled regexes for operatorname fixing and SVG color injection.
static RE_OPERATORNAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\?operatorname\{([^}]+)\}").unwrap());
static RE_FILL_BLACK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##"fill="#000000?""##).unwrap());
/// MiTeX's name for ∂, which Typst now calls `partial`. Word-bounded so it
/// cannot chew through an identifier that merely contains "diff".
static RE_DIFF_SYMBOL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bdiff\b").unwrap());
static RE_STROKE_BLACK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##"stroke="#000000?""##).unwrap());

/// Simple RGB color representation for theme colors (avoids gpui dependency).
/// Each component is a float in the range [0.0, 1.0].
#[derive(Clone, Copy, Debug)]
pub struct RgbColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};

// Typst margins for math rendering (in points)
// Inline math sits between words: the text's own spaces separate it, so any
// horizontal margin here only widens the gap.
const INLINE_MARGIN_X: f64 = 0.5;
// Room for what overflows the line box: limits under a sum, a fraction's
// denominator. Typst sizes the page to the line, so less than this clips them.
const INLINE_MARGIN_Y: f64 = 6.0;
// Inline math is set at this size so its glyphs match the transcript's text
// once the SVG is scaled by `SVG_SCALE_FACTOR`. At Typst's default 11pt it
// came out about half again as large as the words around it.
const INLINE_FONT_SIZE_PT: f64 = 7.5;
const BLOCK_MARGIN_X: f64 = 8.0;
const BLOCK_MARGIN_Y: f64 = 10.0;

// SVG scaling factor for high-DPI displays
const SVG_SCALE_FACTOR: f64 = 1.5;

/// Replace every `name( … )` call in MiTeX output with its argument. Escaped
/// parentheses (`\(`, `\)`) are literal characters, not grouping. The name
/// must stand alone, so `aligned` does not match inside `alignedat`.
fn unwrap_call(code: &str, name: &str) -> String {
    let pattern = format!("{name}(");
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(at) = rest.find(&pattern) {
        let standalone = rest[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '.');
        let body_start = at + pattern.len();
        let close = standalone
            .then(|| matching_paren(&rest[body_start..]))
            .flatten();
        let Some(close) = close else {
            out.push_str(&rest[..body_start]);
            rest = &rest[body_start..];
            continue;
        };
        out.push_str(&rest[..at]);
        out.push_str(&rest[body_start..body_start + close]);
        rest = &rest[body_start + close + 1..];
    }
    out.push_str(rest);
    out
}

/// Byte index of the `)` closing an already-open group in `s`.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Typst's embedded fonts, parsed once per process. Every compile used to
/// re-parse all of them in `MathWorld::new` (AGE-394); a `Font` is an `Arc`
/// over the parsed face, so handing each world a clone is cheap.
static FONTS: LazyLock<(Vec<Font>, FontBook)> = LazyLock::new(|| {
    let fonts = typst_assets::fonts()
        .map(|data| Font::new(Bytes::new(data), 0).unwrap())
        .collect::<Vec<_>>();
    let book = FontBook::from_fonts(fonts.iter());
    (fonts, book)
});

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

        let (fonts, book) = &*FONTS;
        let fonts = fonts.clone();
        let book = LazyHash::new(book.clone());

        // Create virtual file ID for the main file
        let vpath = VirtualPath::new("main.typ").expect("valid virtual path");
        let main_id = FileId::new(RootedPath::new(VirtualRoot::Project, vpath));

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
            Err(FileError::NotFound(id.vpath().get_without_slash().into()))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(id.vpath().get_without_slash().into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        Some(Datetime::from_ymd(2024, 1, 1).unwrap())
    }
}

// Maximum number of in-memory SVG cache entries. Prevents unbounded memory growth
// in long sessions with many unique math expressions. At ~10-50KB per SVG, 200
// entries ≈ 2-10MB worst case. Disk cache provides persistence for evicted entries.
const MAX_MATH_CACHE_ENTRIES: usize = 200;

/// Math renderer service that converts LaTeX to SVG using Typst
pub struct MathRendererService {
    cache: Arc<Mutex<HashMap<String, String>>>,
    /// Tracks insertion order for LRU eviction of in-memory cache entries.
    insertion_order: Arc<Mutex<VecDeque<String>>>,
    /// The persistent SVG cache directory, resolved once at construction
    /// rather than on every lookup. `None` when there is no config directory.
    cache_dir: Option<PathBuf>,
    /// Panic on any `render_*` call. Lets a test prove a code path — the
    /// transcript's render path (AGE-394) — never renders math.
    #[cfg(any(test, feature = "test-support"))]
    panic_on_render: bool,
}

impl Default for MathRendererService {
    fn default() -> Self {
        Self::new()
    }
}

impl MathRendererService {
    /// Fix \operatorname{...} by converting to op("...") for Typst
    fn fix_operatorname(code: &str) -> String {
        let mut result = code.to_string();

        // Pattern 1: \operatorname{name} or operatorname{name}
        result = RE_OPERATORNAME
            .replace_all(&result, |caps: &regex::Captures| {
                let name = &caps[1];
                format!(r#"op("{}")"#, name)
            })
            .to_string();

        // Pattern 2: Plain "operatorname" keyword (MiTeX might strip backslash and braces)
        result = result.replace("operatorname", "op");

        result
    }

    pub fn new() -> Self {
        Self::with_cache_dir_opt(Self::cache_dir().ok())
    }

    /// A service whose persistent SVG cache lives in `dir` instead of the
    /// user's config directory, so a test never writes to `~/.config`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_cache_dir(dir: PathBuf) -> Self {
        Self::with_cache_dir_opt(Some(dir))
    }

    /// A service that panics on any `render_*` call (see `panic_on_render`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn panicking_stub() -> Self {
        Self {
            panic_on_render: true,
            ..Self::with_cache_dir_opt(None)
        }
    }

    fn with_cache_dir_opt(cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            insertion_order: Arc::new(Mutex::new(VecDeque::new())),
            cache_dir,
            #[cfg(any(test, feature = "test-support"))]
            panic_on_render: false,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn assert_may_render(&self, latex: &str) {
        assert!(
            !self.panic_on_render,
            "MathRendererService::render_* called on a path that must not render math: {latex}"
        );
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn assert_may_render(&self, _latex: &str) {}

    /// Render LaTeX math expression to SVG
    pub fn render_to_svg(&self, latex: &str, is_inline: bool) -> Result<String> {
        self.assert_may_render(latex);
        let cache_key = self.make_cache_key(latex, is_inline);
        self.render_to_svg_keyed(&cache_key, latex, is_inline)
    }

    /// `render_to_svg` for a caller that already computed the cache key.
    fn render_to_svg_keyed(&self, cache_key: &str, latex: &str, is_inline: bool) -> Result<String> {
        // Check cache first
        if let Ok(cache) = self.cache.lock()
            && let Some(svg) = cache.get(cache_key)
        {
            debug!(latex, "Math cache hit");
            let svg = svg.clone();
            // Touch LRU order so this entry stays fresh
            if let Ok(mut order) = self.insertion_order.lock() {
                order.retain(|k| k != cache_key);
                order.push_back(cache_key.to_string());
            }
            return Ok(svg);
        }

        debug!(latex, is_inline, "Rendering math to SVG");

        // Convert LaTeX to Typst using MiTeX
        let mut typst_code = mitex::convert_math(latex, None)
            .map_err(|e| anyhow::anyhow!("Failed to convert LaTeX to Typst: {} - {}", latex, e))?;

        // Fix MiTeX bugs - MiTeX generates invalid Typst functions
        typst_code = typst_code.replace("mitexsqrt", "sqrt");
        typst_code = typst_code.replace("mitexmathbf", "bold");
        typst_code = typst_code.replace("tfrac", "frac");
        typst_code = typst_code.replace("dfrac", "frac");
        typst_code = typst_code.replace("pmatrix", "mat");
        // MiTeX wraps `aligned` / `gathered` in helper functions Typst does
        // not have. Typst math aligns on `&` and breaks on `\` by itself, so
        // the body is all that is needed. (Swapping in `cases` drew a stray
        // brace and ran the rows together.)
        for env in ["aligned", "gathered"] {
            typst_code = unwrap_call(&typst_code, env);
        }

        // MiTeX 0.2.4 emits symbol names from an older Typst. Two of them no
        // longer resolve against the Typst we compile with, and each one kills
        // the whole equation:
        //   `\hbar`    -> `planck.reduce`, but `planck` is ħ itself now and
        //                 has no `reduce` modifier ("unknown symbol modifier")
        //   `\partial` -> `diff`, which no longer exists ("unknown variable")
        // Both are plain renames; check `codex`'s `sym.txt` when adding more.
        typst_code = typst_code.replace("planck.reduce", "planck");
        typst_code = RE_DIFF_SYMBOL
            .replace_all(&typst_code, "partial")
            .into_owned();

        // Fix \operatorname{...} - convert to op("...") for Typst
        // MiTeX may pass through \operatorname as-is or convert it incorrectly
        if typst_code.contains("operatorname") {
            debug!(before_operatorname_fix = %typst_code, "Found operatorname in Typst code");
            typst_code = Self::fix_operatorname(&typst_code);
            debug!(after_operatorname_fix = %typst_code, "After operatorname fix");
        }

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

        debug!(typst_code = %typst_code, "MiTeX converted LaTeX to Typst (after fixes)");

        // Wrap in Typst document template with minimal page size
        // `fill: none` is required, not cosmetic. Typst's page fill defaults to
        // `Auto`, which SVG export resolves to *white* (`fill_or_white`), so the
        // exported file opens with a full-canvas white rect. Colour injection
        // only rewrites the black glyph fills, so that rect survived into the
        // transcript and math rendered on a white card in dark themes.
        let doc_content = if is_inline {
            format!("#set page(width: auto, height: auto, fill: none, margin: (x: {INLINE_MARGIN_X}pt, y: {INLINE_MARGIN_Y}pt))
#set text(size: {INLINE_FONT_SIZE_PT}pt)
${typst_code}$")
        } else {
            // Spaces around content make it display math
            format!("#set page(width: auto, height: auto, fill: none, margin: (x: {BLOCK_MARGIN_X}pt, y: {BLOCK_MARGIN_Y}pt))
$ {typst_code} $")
        };

        // Compile with Typst
        let svg = self
            .compile_typst_to_svg(&doc_content)
            .context("Failed to compile Typst to SVG")?;

        // Store in cache with LRU eviction
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key.to_string(), svg.clone());
            if let Ok(mut order) = self.insertion_order.lock() {
                order.retain(|k| k != cache_key);
                order.push_back(cache_key.to_string());
                while cache.len() > MAX_MATH_CACHE_ENTRIES {
                    if let Some(oldest) = order.pop_front() {
                        cache.remove(&oldest);
                    } else {
                        break;
                    }
                }
            } else {
                warn!("Failed to lock math cache insertion_order — entry may not be evicted");
            }
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
    ///
    /// The disk cache is checked *before* the in-memory one: the in-memory
    /// cache holds at most `MAX_MATH_CACHE_ENTRIES` SVGs, and looking there
    /// first meant a message with more distinct equations than that
    /// recompiled the overflow with Typst on every pass even though its SVG
    /// was already on disk (AGE-394).
    pub fn render_to_svg_file(&self, latex: &str, is_inline: bool) -> Result<PathBuf> {
        self.assert_may_render(latex);
        let cache_dir = self
            .cache_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No config directory"))?;

        // Use hash as filename for deterministic caching
        let cache_key = self.make_cache_key(latex, is_inline);
        let svg_path = cache_dir.join(format!("{}.svg", cache_key));

        if svg_path.exists() {
            debug!(path = ?svg_path, "Math SVG cache hit");
            return Ok(svg_path);
        }

        // Get or generate SVG (uses existing in-memory cache)
        let svg_data = self.render_to_svg_keyed(&cache_key, latex, is_inline)?;

        std::fs::create_dir_all(cache_dir).context("Failed to create math cache directory")?;

        // Strip width/height attributes from SVG to allow GPUI to scale it
        // Typst generates SVGs with small pt dimensions that GPUI respects literally
        let svg_without_dims = self.strip_svg_dimensions(&svg_data);

        std::fs::write(&svg_path, svg_without_dims).context("Failed to write SVG to cache")?;
        info!(path = ?svg_path, "Wrote math SVG to persistent cache");

        Ok(svg_path)
    }

    /// Render LaTeX to styled SVG file with theme color injected
    ///
    /// This method pre-computes styled SVG variants for different theme colors.
    /// Styled SVGs are cached on disk with filenames like: {hash}.styled.{color_hash}.svg
    ///
    /// This allows theme switching without re-rendering or re-injecting colors
    /// during the render phase, significantly improving performance.
    ///
    /// Called at parse time, when a message's parse result is built and
    /// cached — never from the transcript's render path, which only builds
    /// `img(path)` from the stored result (AGE-394).
    ///
    /// Returns the PathBuf to the styled SVG file.
    pub fn render_to_styled_svg_file(
        &self,
        latex: &str,
        is_inline: bool,
        theme_color: RgbColor,
    ) -> Result<PathBuf> {
        use sha2::Digest;

        // 1. Generate base SVG (uses existing cache)
        let base_svg_path = self.render_to_svg_file(latex, is_inline)?;

        // 2. Compute color hash for styled variant filename
        let hex_color = format!(
            "{:02x}{:02x}{:02x}",
            (theme_color.r * 255.0) as u8,
            (theme_color.g * 255.0) as u8,
            (theme_color.b * 255.0) as u8
        );

        let mut hasher = Sha256::new();
        hasher.update(hex_color.as_bytes());
        let color_hash = hex::encode(hasher.finalize());

        // 3. Build styled variant path
        let base_name = base_svg_path
            .file_stem()
            .ok_or_else(|| anyhow::anyhow!("Invalid SVG path"))?
            .to_string_lossy();

        let styled_path =
            base_svg_path.with_file_name(format!("{}.styled.{}.svg", base_name, &color_hash[..16]));

        // 4. Return cached styled variant if it exists
        if styled_path.exists() {
            debug!(path = ?styled_path, "Styled SVG cache hit");
            return Ok(styled_path);
        }

        // 5. Read base SVG, inject color, write styled variant
        let svg_content =
            std::fs::read_to_string(&base_svg_path).context("Failed to read base SVG")?;

        let styled_svg = self.inject_svg_color(&svg_content, theme_color);

        std::fs::write(&styled_path, styled_svg).context("Failed to write styled SVG")?;

        info!(path = ?styled_path, color = %hex_color, "Created styled SVG variant");

        Ok(styled_path)
    }

    /// Inject theme color into SVG by replacing black color attributes
    ///
    /// Replaces inline `fill`/`stroke` attributes that use six-digit black
    /// (`#000000`) with the provided theme color. It does not inject CSS and
    /// does not touch backgrounds — the page is kept transparent at the Typst
    /// end instead (`fill: none`). This approach:
    /// - Preserves all non-black colors and attributes like fill="none"
    /// - Avoids CSS selector warnings from the SVG rendering library
    /// - Works for all math elements including fraction lines
    fn inject_svg_color(&self, svg_content: &str, color: RgbColor) -> String {
        let hex_color = format!(
            "#{:02x}{:02x}{:02x}",
            (color.r * 255.0) as u8,
            (color.g * 255.0) as u8,
            (color.b * 255.0) as u8
        );

        let fill_replacement = format!(r#"fill="{}""#, hex_color);
        let stroke_replacement = format!(r#"stroke="{}""#, hex_color);

        // Use Cow chaining to avoid intermediate String allocations when
        // no replacements are made (common for already-themed SVGs).
        let after_fill = RE_FILL_BLACK.replace_all(svg_content, fill_replacement.as_str());
        let after_stroke = RE_STROKE_BLACK.replace_all(&after_fill, stroke_replacement.as_str());
        after_stroke.into_owned()
    }

    /// Cache version — bump whenever the generated Typst or the SVG
    /// post-processing changes, to invalidate on-disk SVGs from older builds.
    ///
    /// Base `{hash}.svg` files are written once and never cleaned (only the
    /// `.styled.` variants are), so without this a rendering fix reaches new
    /// installs only.
    const CACHE_VERSION: &'static str = "v4";

    fn make_cache_key(&self, latex: &str, is_inline: bool) -> String {
        let mut hasher = Sha256::new();
        hasher.update(Self::CACHE_VERSION.as_bytes());
        hasher.update(latex.as_bytes());
        hasher.update(if is_inline { b"inline" } else { b"block " });
        hex::encode(hasher.finalize())
    }

    /// Compile Typst source to SVG
    fn compile_typst_to_svg(&self, typst_content: &str) -> Result<String> {
        // Create a minimal World for this compilation
        let world = MathWorld::new(typst_content);

        // Compile the document
        let warned_result = typst::compile::<typst_layout::PagedDocument>(&world);

        // Extract the document, handling any errors
        let document = warned_result.output.map_err(|errors| {
            let error_messages: Vec<String> =
                errors.iter().map(|e| format!("{}", e.message)).collect();
            anyhow::anyhow!("Typst compilation failed: {}", error_messages.join(", "))
        })?;

        // Render the first (and only) page to SVG
        let page = document
            .pages()
            .first()
            .ok_or_else(|| anyhow::anyhow!("Typst compilation produced no pages"))?;
        let svg_data = typst_svg::svg(page, &typst_svg::SvgOptions::default());

        Ok(svg_data)
    }

    /// Clear the math rendering cache (tested in tests::test_clear_cache)
    #[allow(dead_code)]
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        if let Ok(mut order) = self.insertion_order.lock() {
            order.clear();
        }
    }

    /// Strip width and height attributes from SVG and add proper scaling
    fn strip_svg_dimensions(&self, svg: &str) -> String {
        // Remove width="..." and height="..." attributes
        // Keep viewBox as it's needed for aspect ratio
        let mut result = svg.to_string();

        // Find and remove width attribute
        if let Some(width_start) = result.find(r#" width=""#)
            && let Some(width_end) = result[width_start..].find('"')
        {
            // Find the closing quote
            let quote_pos = width_start + width_end + 1;
            if let Some(closing_quote) = result[quote_pos..].find('"') {
                result.replace_range(width_start..quote_pos + closing_quote + 1, "");
            }
        }

        // Find and remove height attribute
        if let Some(height_start) = result.find(r#" height=""#)
            && let Some(height_end) = result[height_start..].find('"')
        {
            // Find the closing quote
            let quote_pos = height_start + height_end + 1;
            if let Some(closing_quote) = result[quote_pos..].find('"') {
                result.replace_range(height_start..quote_pos + closing_quote + 1, "");
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
                    if let (Ok(width), Ok(height)) =
                        (parts[2].parse::<f64>(), parts[3].parse::<f64>())
                    {
                        // Scale by 1.5x for better readability (matches typical font sizes)
                        let scaled_width = width * SVG_SCALE_FACTOR;
                        let scaled_height = height * SVG_SCALE_FACTOR;

                        // Insert width and height attributes after viewBox
                        let insert_pos = vb_start + vb_end + 1; // after closing quote of viewBox
                        let size_attrs = format!(
                            r#" width="{}pt" height="{}pt""#,
                            scaled_width, scaled_height
                        );
                        result.insert_str(insert_pos, &size_attrs);
                    }
                }
            }
        }

        result
    }

    /// Get the cache directory path
    fn cache_dir() -> Result<PathBuf> {
        let cache_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("No config directory"))?
            .join("chatty")
            .join("math_cache");
        Ok(cache_dir)
    }

    /// Cleans up old styled SVG variants from previous sessions
    ///
    /// Keeps base SVG files (no "styled" in filename) but removes theme variants.
    /// This prevents unbounded disk usage from accumulating styled SVG files
    /// as users switch themes.
    pub fn cleanup_old_styled_svgs() -> Result<()> {
        let cache_dir = Self::cache_dir()?;

        if !cache_dir.exists() {
            return Ok(());
        }

        let mut removed_count = 0;

        for entry in std::fs::read_dir(&cache_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                // Remove files matching pattern: {hash}.styled.{color_hash}.svg
                if filename.contains(".styled.") && filename.ends_with(".svg") {
                    std::fs::remove_file(&path)?;
                    removed_count += 1;
                }
            }
        }

        if removed_count > 0 {
            info!(count = removed_count, "Cleaned up old styled math SVGs");
        }

        Ok(())
    }

    /// Get the number of cached items (tested in tests::test_cache_*)
    #[allow(dead_code)]
    pub fn cache_size(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn aligned_is_unwrapped_not_turned_into_cases() {
        let code = "aligned( \\(a + b \\)^(2 ) &=  a ^(2 ) \\  &=  b )";
        assert_eq!(
            super::unwrap_call(code, "aligned"),
            " \\(a + b \\)^(2 ) &=  a ^(2 ) \\  &=  b "
        );
        // Not a standalone call: left alone.
        assert_eq!(
            super::unwrap_call("alignedat(x)", "aligned"),
            "alignedat(x)"
        );
        // Unclosed: left alone.
        assert_eq!(super::unwrap_call("aligned( x", "aligned"), "aligned( x");
    }

    #[test]
    fn an_aligned_derivation_renders() {
        let service = MathRendererService::new();
        let svg = service
            .render_to_svg(
                "\\begin{aligned} (a+b)^2 &= (a+b)(a+b) \\\\ &= a^2 + 2ab + b^2 \\end{aligned}",
                false,
            )
            .expect("aligned renders");
        assert!(svg.contains("<svg"));
    }

    use super::*;

    /// MiTeX targets an older Typst than we compile with, and a stale symbol
    /// name fails the whole equation rather than one glyph. These two were
    /// reported from a real conversation (Schrödinger, Maxwell).
    #[test]
    fn renders_math_using_symbols_mitex_names_for_an_older_typst() {
        let service = MathRendererService::new();
        for latex in [
            r"i\hbar \frac{\partial}{\partial t}\Psi(\mathbf{r},t) = \left[ -\frac{\hbar^2}{2m}\nabla^2 + V(\mathbf{r},t) \right] \Psi(\mathbf{r},t)",
            r"\nabla \times \mathbf{E} = -\frac{\partial \mathbf{B}}{\partial t}",
            r"\hbar",
            r"\partial",
        ] {
            service
                .render_to_svg(latex, false)
                .unwrap_or_else(|e| panic!("{latex} failed to render: {e:#}"));
        }
    }

    #[test]
    fn test_cache_starts_empty() {
        let service = MathRendererService::new();
        assert_eq!(service.cache_size(), 0);
    }

    #[test]
    fn test_cache_populates_after_render() {
        let service = MathRendererService::new();
        service.render_to_svg("x^2", true).unwrap();
        assert_eq!(service.cache_size(), 1);
    }

    /// The transcript is dark; Typst's page fill defaults to white for SVG
    /// export, and colour injection only rewrites glyph fills. A white page
    /// rect here is a white card behind every equation.
    #[test]
    fn rendered_math_has_no_opaque_page_background() {
        let service = MathRendererService::new();
        for (latex, inline) in [
            ("x^2", true),
            ("\\sum_{n=1}^{\\infty} \\frac{1}{n^2}", false),
        ] {
            let svg = service.render_to_svg(latex, inline).expect("render");
            assert!(
                !svg.contains("#ffffff"),
                "math SVG carries an opaque page background (inline={inline})"
            );
        }
    }

    #[test]
    fn test_cache_deduplicates_same_input() {
        let service = MathRendererService::new();
        service.render_to_svg("x^2", true).unwrap();
        service.render_to_svg("x^2", true).unwrap();
        assert_eq!(service.cache_size(), 1);
    }

    #[test]
    fn test_cache_distinguishes_inline_vs_block() {
        let service = MathRendererService::new();
        service.render_to_svg("x^2", true).unwrap();
        service.render_to_svg("x^2", false).unwrap();
        assert_eq!(service.cache_size(), 2);
    }

    #[test]
    fn test_clear_cache() {
        let service = MathRendererService::new();
        service.render_to_svg("x^2", true).unwrap();
        service.render_to_svg("y^2", false).unwrap();
        assert_eq!(service.cache_size(), 2);

        service.clear_cache();
        assert_eq!(service.cache_size(), 0);
    }

    #[test]
    fn test_render_simple_inline_math() {
        let service = MathRendererService::new();
        let svg = service.render_to_svg("x^2 + y^2", true).unwrap();
        assert!(svg.contains("<svg"), "Output should be SVG");
    }

    #[test]
    fn test_render_block_math() {
        let service = MathRendererService::new();
        let svg = service.render_to_svg("\\frac{a}{b}", false).unwrap();
        assert!(svg.contains("<svg"), "Output should be SVG");
    }

    /// AGE-394: the disk cache is consulted before the in-memory one, so an
    /// equation whose SVG is already on disk is never recompiled — not even
    /// after the bounded in-memory cache has evicted it.
    #[test]
    fn disk_cached_svg_is_served_without_recompiling() {
        let dir = tempfile::tempdir().unwrap();
        let service = MathRendererService::with_cache_dir(dir.path().to_path_buf());

        let path = service.render_to_svg_file("x^2", true).unwrap();
        assert!(path.exists());
        assert_eq!(service.cache_size(), 1, "the first render compiles");

        // Simulate eviction of the in-memory entry (or a fresh process).
        service.clear_cache();
        let again = service.render_to_svg_file("x^2", true).unwrap();
        assert_eq!(again, path);
        assert_eq!(
            service.cache_size(),
            0,
            "a disk hit must not compile (a compile would repopulate the in-memory cache)"
        );

        // The styled variant is likewise a pure path lookup once written.
        let color = RgbColor {
            r: 1.0,
            g: 0.5,
            b: 0.0,
        };
        let styled = service
            .render_to_styled_svg_file("x^2", true, color)
            .unwrap();
        let styled_again = service
            .render_to_styled_svg_file("x^2", true, color)
            .unwrap();
        assert_eq!(styled, styled_again);
        assert_eq!(service.cache_size(), 0);
    }

    /// The stub must actually bite, or a test built on it proves nothing.
    #[test]
    #[should_panic(expected = "must not render math")]
    fn panicking_stub_panics_on_render() {
        MathRendererService::panicking_stub()
            .render_to_styled_svg_file(
                "x^2",
                true,
                RgbColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                },
            )
            .ok();
    }

    #[test]
    fn test_cache_max_entries_constant_is_reasonable() {
        const {
            assert!(
                MAX_MATH_CACHE_ENTRIES >= 100,
                "Cache limit too low — would cause excessive re-renders"
            );
            assert!(
                MAX_MATH_CACHE_ENTRIES <= 2000,
                "Cache limit too high — defeats memory bounding"
            );
        }
    }
}
