use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use super::math_parser::MathSegment;
use super::syntax_highlighter::HighlightedSpan;

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

/// Cached segments for a code block with pre-computed syntax highlighting
#[derive(Clone, Debug)]
pub struct CachedCodeBlock {
    pub language: Option<String>,
    pub code: String,
    pub highlighted_spans: Vec<HighlightedSpan>,
}

/// One segment of rendered content — either text (possibly containing math) or a code block
#[derive(Clone, Debug)]
pub enum CachedMarkdownSegment {
    /// Text that may contain math — stores pre-parsed math segments
    TextWithMath(Vec<MathSegment>),
    /// Code block with pre-computed syntax highlighting
    CodeBlock(CachedCodeBlock),
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

/// Cache for parsed message content, keyed by content hash + theme
pub struct ParsedContentCache {
    entries: HashMap<ContentCacheKey, CachedParseResult>,
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
        }
    }

    pub fn get(&self, key: &ContentCacheKey) -> Option<&CachedParseResult> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: ContentCacheKey, result: CachedParseResult) {
        self.entries.insert(key, result);
    }

    /// Clear the entire cache (e.g., on conversation switch)
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
