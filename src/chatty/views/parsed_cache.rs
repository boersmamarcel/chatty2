use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use super::math_parser::MathSegment;
use gpui::HighlightStyle;

/// Maximum number of entries before oldest are evicted.
const MAX_ENTRIES: usize = 200;

/// A content hash used as cache key, computed from message content + theme mode
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentCacheKey(u64);

impl ContentCacheKey {
    pub fn new(content: &str, is_dark_theme: bool) -> Self {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        is_dark_theme.hash(&mut hasher);
        Self(hasher.finish())
    }
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
    /// Text that may contain math — stores pre-parsed math segments
    TextWithMath(Vec<MathSegment>),
    /// Code block with pre-computed syntax highlighting
    CodeBlock(CachedCodeBlock),
    /// Incomplete code block (opening ``` without closing ```) during streaming.
    /// Rendered as plain monospace text without syntax highlighting.
    IncompleteCodeBlock {
        language: Option<String>,
        code: String,
    },
    /// Rendered Mermaid diagram with pre-computed SVG path.
    /// `svg_path` is None if rendering failed (falls back to raw source display).
    MermaidDiagram {
        source: String,
        svg_path: Option<PathBuf>,
    },
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
            let key = ContentCacheKey::new(&format!("message-{i}"), false);
            cache.insert(key, dummy_result());
        }

        // Cache should be bounded to MAX_ENTRIES
        assert_eq!(cache.len(), MAX_ENTRIES);

        // Oldest 10 entries should be evicted
        for i in 0..10 {
            let key = ContentCacheKey::new(&format!("message-{i}"), false);
            assert!(cache.get(&key).is_none(), "entry {i} should be evicted");
        }

        // Newest entries should still be present
        for i in 10..(MAX_ENTRIES + 10) {
            let key = ContentCacheKey::new(&format!("message-{i}"), false);
            assert!(cache.get(&key).is_some(), "entry {i} should be present");
        }
    }

    #[test]
    fn test_duplicate_insert_no_double_track() {
        let mut cache = ParsedContentCache::new();
        let key = ContentCacheKey::new("same content", false);

        cache.insert(key, dummy_result());
        cache.insert(key, dummy_result());

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.insertion_order.len(), 1);
    }

    #[test]
    fn test_clear_resets_both_structures() {
        let mut cache = ParsedContentCache::new();
        for i in 0..5 {
            let key = ContentCacheKey::new(&format!("msg-{i}"), false);
            cache.insert(key, dummy_result());
        }

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.insertion_order.is_empty());
    }
}
