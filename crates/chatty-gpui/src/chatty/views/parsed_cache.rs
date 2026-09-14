use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use chatty_core::services::math_renderer_service::RgbColor;
use gpui::{App, HighlightStyle};
use gpui_component::ActiveTheme;
use rustc_hash::FxHasher;

/// Maximum number of entries before oldest are evicted.
const MAX_ENTRIES: usize = 200;

/// The theme inputs a parse result bakes in: mermaid picks its palette by
/// mode, and every equation's styled SVG path carries the foreground colour
/// (AGE-394). A result built under one theme is wrong under another, so both
/// are part of [`ContentCacheKey`].
#[derive(Clone, Copy, Debug)]
pub struct ThemeKey {
    pub is_dark: bool,
    pub foreground: RgbColor,
}

impl ThemeKey {
    pub fn current(cx: &App) -> Self {
        let rgb = cx.theme().foreground.to_rgb();
        Self {
            is_dark: cx.theme().mode.is_dark(),
            foreground: RgbColor {
                r: rgb.r,
                g: rgb.g,
                b: rgb.b,
            },
        }
    }

    /// The foreground quantised the way the math service names styled SVGs.
    fn foreground_bytes(&self) -> [u8; 3] {
        let c = self.foreground;
        [
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
        ]
    }
}

/// A content hash used as cache key, computed from message content + theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentCacheKey(u64);

impl ContentCacheKey {
    /// `FxHasher`, not `DefaultHasher`: this hashes the whole message on every
    /// frame it is visible, nothing here is attacker-chosen, and SipHash's
    /// keyed mixing was the bulk of the lookup's cost — the AGE-375 finding
    /// for the turn cache, again (AGE-394).
    pub fn new(content: &str, theme: ThemeKey) -> Self {
        let mut hasher = FxHasher::default();
        content.hash(&mut hasher);
        theme.is_dark.hash(&mut hasher);
        theme.foreground_bytes().hash(&mut hasher);
        Self(hasher.finish())
    }
}

/// [`MathSegment`](super::math_parser::MathSegment) with each equation's
/// styled SVG resolved at parse time, so the render path only builds
/// `img(path)` — no service lookup, digest or `stat` per equation per frame
/// (AGE-394). `svg_path` is `None` when rendering failed (falls back to the
/// raw LaTeX), mirroring `MermaidDiagram`.
#[derive(Clone, Debug)]
pub enum CachedMathSegment {
    /// Regular text content (may contain markdown)
    Text(String),
    InlineMath {
        latex: String,
        svg_path: Option<PathBuf>,
    },
    BlockMath {
        latex: String,
        svg_path: Option<PathBuf>,
    },
}

/// Cached segments for a code block with pre-computed syntax highlighting.
///
/// Styles are byte-range offsets into `code` paired with `HighlightStyle`.
#[derive(Clone, Debug)]
pub struct CachedCodeBlock {
    pub language: Option<String>,
    pub code: String,
    pub styles: Vec<(std::ops::Range<usize>, HighlightStyle)>,
}

/// One segment of rendered content — either text (possibly containing math) or a code block
#[derive(Clone, Debug)]
pub enum CachedMarkdownSegment {
    /// Text that may contain math — stores pre-parsed math segments with
    /// their SVGs already resolved
    TextWithMath(Vec<CachedMathSegment>),
    /// Code block with pre-computed syntax highlighting
    CodeBlock(CachedCodeBlock),
    /// Incomplete code block (opening ``` without closing ```) during streaming.
    /// Rendered as plain monospace text without syntax highlighting.
    IncompleteCodeBlock {
        language: Option<String>,
        code: String,
    },
    /// Unclosed code block after streaming has ended.
    /// Rendered as finalized plain code without syntax highlighting.
    UnclosedCodeBlock {
        language: Option<String>,
        code: String,
    },
    /// Rendered Mermaid diagram with pre-computed SVG path.
    /// `svg_path` is None if rendering failed (falls back to raw source display).
    MermaidDiagram {
        source: String,
        svg_path: Option<PathBuf>,
    },
    /// The line still being streamed: everything after the last newline of
    /// the growing text segment. Rendered as plain text — no markdown parse,
    /// no math parse, no Typst — until a newline lands or the stream ends,
    /// at which point it is promoted like any other text (AGE-167).
    PlainTail(String),
}

/// Cached result for a single content segment (after think-block extraction)
#[derive(Clone, Debug)]
pub enum CachedContentSegment {
    /// Regular text content, parsed through markdown + math layers
    Text(Vec<CachedMarkdownSegment>),
    /// A thinking block (content is plain text, no further parsing needed)
    Thinking(String),
}

/// The fully cached parse result for one message
#[derive(Clone, Debug)]
pub struct CachedParseResult {
    pub segments: Vec<CachedContentSegment>,
}

/// State for incremental streaming parse, tracking metadata to enable
/// stable-prefix reuse across renders.
///
/// During streaming, content only grows at the end. By comparing segment
/// counts with the previous render, we can reuse all stable segments and
/// only re-parse the growing tail.
#[derive(Clone, Debug)]
pub struct StreamingParseState {
    pub result: CachedParseResult,
    /// Total byte length of content that produced this result.
    pub content_len: usize,
    /// Number of content segments (from `parse_content_segments`).
    pub content_segment_count: usize,
    /// Markdown segment count in the last Text content segment.
    pub last_text_md_count: usize,
    /// Math parse of the settled part of the growing text segment, reused
    /// while that prefix is unchanged (AGE-167).
    pub live_tail: Option<LiveTailCache>,
}

/// The settled prefix of the streaming text segment and its math parse.
///
/// Only the tail after the last newline changes from one text batch to the
/// next, so the prefix's resolved math parse is kept and handed out
/// by pointer until a newline moves the split.
#[derive(Clone, Debug)]
pub struct LiveTailCache {
    /// The text up to and including the last newline.
    pub settled_text: String,
    pub settled: Arc<Vec<CachedMathSegment>>,
}

/// Bounded cache for parsed message content, keyed by content hash + theme.
///
/// Evicts the oldest entries (by insertion order) when the cache exceeds
/// `MAX_ENTRIES`. This keeps memory bounded in long conversations while
/// retaining the most recently viewed messages.
pub struct ParsedContentCache {
    entries: HashMap<ContentCacheKey, CachedParseResult>,
    insertion_order: VecDeque<ContentCacheKey>,
}

impl Default for ParsedContentCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ParsedContentCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    pub fn get(&self, key: &ContentCacheKey) -> Option<&CachedParseResult> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: ContentCacheKey, result: CachedParseResult) {
        // Only track insertion order for genuinely new keys
        if self.entries.insert(key, result).is_none() {
            self.insertion_order.push_back(key);
        }

        // Evict oldest entries if over budget
        while self.entries.len() > MAX_ENTRIES {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Clear the entire cache (e.g., on conversation switch)
    pub fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }

    /// Number of cached entries.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: ThemeKey = ThemeKey {
        is_dark: false,
        foreground: RgbColor {
            r: 0.1,
            g: 0.1,
            b: 0.1,
        },
    };

    fn dummy_result() -> CachedParseResult {
        CachedParseResult {
            segments: vec![CachedContentSegment::Thinking("test".to_string())],
        }
    }

    #[test]
    fn test_eviction_at_max_entries() {
        let mut cache = ParsedContentCache::new();

        // Insert MAX_ENTRIES + 10 entries
        for i in 0..(MAX_ENTRIES + 10) {
            let key = ContentCacheKey::new(&format!("message-{i}"), LIGHT);
            cache.insert(key, dummy_result());
        }

        // Cache should be bounded to MAX_ENTRIES
        assert_eq!(cache.len(), MAX_ENTRIES);

        // Oldest 10 entries should be evicted
        for i in 0..10 {
            let key = ContentCacheKey::new(&format!("message-{i}"), LIGHT);
            assert!(cache.get(&key).is_none(), "entry {i} should be evicted");
        }

        // Newest entries should still be present
        for i in 10..(MAX_ENTRIES + 10) {
            let key = ContentCacheKey::new(&format!("message-{i}"), LIGHT);
            assert!(cache.get(&key).is_some(), "entry {i} should be present");
        }
    }

    #[test]
    fn test_duplicate_insert_no_double_track() {
        let mut cache = ParsedContentCache::new();
        let key = ContentCacheKey::new("same content", LIGHT);

        cache.insert(key, dummy_result());
        cache.insert(key, dummy_result());

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.insertion_order.len(), 1);
    }

    /// AGE-394: the styled SVG paths inside a parse result depend on the
    /// foreground colour, so two themes of the same mode must not share an
    /// entry.
    #[test]
    fn key_changes_with_foreground_colour_not_just_mode() {
        let other_fg = ThemeKey {
            is_dark: false,
            foreground: RgbColor {
                r: 0.9,
                g: 0.1,
                b: 0.1,
            },
        };
        let dark = ThemeKey {
            is_dark: true,
            ..LIGHT
        };
        assert_ne!(
            ContentCacheKey::new("x", LIGHT),
            ContentCacheKey::new("x", other_fg)
        );
        assert_ne!(
            ContentCacheKey::new("x", LIGHT),
            ContentCacheKey::new("x", dark)
        );
        assert_eq!(
            ContentCacheKey::new("x", LIGHT),
            ContentCacheKey::new("x", LIGHT)
        );
    }

    #[test]
    fn test_clear_resets_both_structures() {
        let mut cache = ParsedContentCache::new();
        for i in 0..5 {
            let key = ContentCacheKey::new(&format!("msg-{i}"), LIGHT);
            cache.insert(key, dummy_result());
        }

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.insertion_order.is_empty());
    }
}
