//! Text parsing and cache-building pipeline for message content.
//!
//! This module transforms raw message text into cached, structured segments
//! ready for rendering. The pipeline has three stages:
//!
//! 1. **Content parsing** ([`parse_content_segments`]): Extract `<think>` blocks
//! 2. **Markdown parsing** ([`parse_markdown_segments`]): Extract fenced code blocks
//! 3. **Cache building** ([`build_cached_parse_result`] / [`build_streaming_parse_result`]):
//!    Apply syntax highlighting, math parsing, and mermaid rendering, producing
//!    [`CachedParseResult`] structs consumed by the rendering layer.
//!
//! Pure parsing functions (`parse_*`) have no GPUI dependency and are independently
//! testable. Build functions require `&App` for syntax highlighting and service access.

use crate::chatty::services::MermaidRendererService;
use gpui::App;
use gpui_component::ActiveTheme;
use regex::Regex;
use std::sync::LazyLock;

use super::math_parser::parse_math_segments;
use super::parsed_cache::{
    CachedCodeBlock, CachedContentSegment, CachedMarkdownSegment, CachedParseResult,
    StreamingParseState,
};
use super::syntax_highlighter;

// ── Types ─────────────────────────────────────────────────────────────────

/// Represents a segment of content - either text or a code block
#[derive(Clone, Debug)]
pub(super) enum MarkdownSegment {
    Text(String),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    /// Incomplete code block (opening ``` without closing ```) detected during streaming.
    IncompleteCodeBlock {
        language: Option<String>,
        code: String,
    },
    /// Unclosed code block after streaming has ended.
    UnclosedCodeBlock {
        language: Option<String>,
        code: String,
    },
}

/// Represents a parsed segment of message content
#[derive(Clone, Debug)]
pub(super) enum ContentSegment {
    /// Regular text content (may contain markdown)
    Text(String),
    /// A thinking block with its content
    Thinking(String),
}

// ── Pure Parsing Functions ────────────────────────────────────────────────

// Regex to match fenced code blocks: ```language\ncode\n```
static CODE_BLOCK_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)```([a-zA-Z0-9_+-]*)
(.*?)
```",
    )
    .expect("CODE_BLOCK_REGEX pattern is valid")
});

/// Parse markdown content into segments of text and code blocks.
///
/// Also detects trailing unclosed code blocks (opening ``` without closing ```).
/// During streaming they are emitted as `IncompleteCodeBlock` segments so they can
/// render in a provisional state; once streaming has ended they are emitted as
/// `UnclosedCodeBlock` segments so they finalize as plain code.
pub(super) fn parse_markdown_segments(content: &str, streaming: bool) -> Vec<MarkdownSegment> {
    let mut segments = Vec::new();
    let mut last_end = 0;

    for cap in CODE_BLOCK_REGEX.captures_iter(content) {
        let match_start = cap.get(0).unwrap().start();
        let match_end = cap.get(0).unwrap().end();

        // Add text before this code block.
        // Trim trailing whitespace to prevent the markdown renderer from
        // creating an extra paragraph break before the code block.
        if match_start > last_end {
            let text = content[last_end..match_start].trim_end().to_string();
            if !text.is_empty() {
                segments.push(MarkdownSegment::Text(text));
            }
        }

        // Add the code block
        let language = cap.get(1).map(|m| m.as_str().to_string());
        let code = cap
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();

        segments.push(MarkdownSegment::CodeBlock {
            language: if language.as_ref().is_some_and(|l| !l.is_empty()) {
                language
            } else {
                None
            },
            code,
        });

        last_end = match_end;
    }

    // Check remaining text for trailing unclosed code blocks.
    if last_end < content.len() {
        let remaining = &content[last_end..];

        if let Some(incomplete) = detect_incomplete_code_block(remaining) {
            // Add text before the incomplete code block.
            // Trim trailing whitespace to prevent extra paragraph breaks.
            let text_before = remaining[..incomplete.0].trim_end();
            if !text_before.is_empty() {
                segments.push(MarkdownSegment::Text(text_before.to_string()));
            }

            if streaming {
                segments.push(MarkdownSegment::IncompleteCodeBlock {
                    language: incomplete.1,
                    code: incomplete.2,
                });
            } else {
                segments.push(MarkdownSegment::UnclosedCodeBlock {
                    language: incomplete.1,
                    code: incomplete.2,
                });
            }
        } else if !remaining.trim().is_empty() {
            segments.push(MarkdownSegment::Text(remaining.to_string()));
        }
    }

    // If no segments were found, return the entire content as text
    if segments.is_empty() {
        segments.push(MarkdownSegment::Text(content.to_string()));
    }

    segments
}

/// Detect an incomplete (unclosed) code block in the given text.
///
/// Returns `Some((offset, language, code))` where `offset` is the byte
/// position of the opening ``` within `text`.
fn detect_incomplete_code_block(text: &str) -> Option<(usize, Option<String>, String)> {
    // Find the last occurrence of ``` that could be an opening fence
    let backtick_pos = text.rfind("```")?;
    let after_backticks = &text[backtick_pos + 3..];

    // Must have a newline after the language tag to be an opening fence
    let newline_pos = after_backticks.find('\n')?;

    let language_str = after_backticks[..newline_pos].trim();
    // Validate language tag (only alphanumeric, _, +, -)
    if !language_str.is_empty()
        && !language_str
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '+' || c == '-')
    {
        return None;
    }

    let language = if language_str.is_empty() {
        None
    } else {
        Some(language_str.to_string())
    };

    let code = after_backticks[newline_pos + 1..].to_string();

    Some((backtick_pos, language, code))
}

/// Parse content to extract thinking blocks and regular text segments.
///
/// Supports `<think>...</think>`, `<thinking>...</thinking>`, `<thought>...</thought>`,
/// and the Gemma4 `<|channel>thought\n...<|channel>model\n` format.
/// Unclosed tags (common during streaming) are treated as incomplete thinking blocks.
pub(super) fn parse_content_segments(content: &str) -> Vec<ContentSegment> {
    let mut segments = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        // Find the earliest opening tag among <thinking>, <thought>, <think>, and Gemma4 <|channel>thought
        let find_thinking = remaining.find("<thinking>").map(|i| (i, 10usize));
        let find_thought = remaining.find("<thought>").map(|i| (i, 9usize));
        // <think> must not be the start of <thinking> (different prefix check isn't needed
        // since <thinking> starts with <think but is longer; find("<think>") won't match
        // inside "<thinking>" because the 8th char is 'i' not '>')
        let find_think = remaining.find("<think>").map(|i| (i, 7usize));
        // Gemma4 thinking channel: <|channel>thought\n
        let find_channel_thought = remaining
            .find("<|channel>thought\n")
            .map(|i| (i, "<|channel>thought\n".len()));

        let result = [
            find_thinking,
            find_thought,
            find_think,
            find_channel_thought,
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(idx, _)| *idx);

        let (start_idx, tag_len) = if let Some(r) = result {
            r
        } else {
            // No more thinking blocks, add remaining text
            let text = remaining.trim();
            if !text.is_empty() {
                segments.push(ContentSegment::Text(text.to_string()));
            }
            break;
        };

        // Add any text before the thinking block
        if start_idx > 0 {
            let text = remaining[..start_idx].trim();
            if !text.is_empty() {
                segments.push(ContentSegment::Text(text.to_string()));
            }
        }

        // Find the closing tag - support </think>, </thinking>, </thought>, and <|channel> (Gemma4)
        let after_open = &remaining[start_idx + tag_len..];
        let end_tag_and_len = after_open
            .find("</think>")
            .map(|idx| (idx, 8)) // "</think>" is 8 chars
            .or_else(|| after_open.find("</thinking>").map(|idx| (idx, 11)))
            .or_else(|| after_open.find("</thought>").map(|idx| (idx, 10)))
            // Gemma4: thinking ends when another <|channel> token starts
            .or_else(|| after_open.find("<|channel>").map(|idx| (idx, 0)));

        if let Some((end_idx, close_tag_len)) = end_tag_and_len {
            let thinking_content = after_open[..end_idx].trim().to_string();
            if !thinking_content.is_empty() {
                segments.push(ContentSegment::Thinking(thinking_content));
            }
            remaining = &after_open[end_idx + close_tag_len..];
        } else {
            // No closing tag found - treat rest as incomplete thinking block (streaming)
            let thinking_content = after_open.trim().to_string();
            if !thinking_content.is_empty() {
                segments.push(ContentSegment::Thinking(thinking_content));
            }
            break;
        }
    }

    segments
}

// ── Cache-Building Pipeline ───────────────────────────────────────────────

/// Run the full three-stage parsing pipeline and return a cacheable result.
///
/// Phases:
/// 1. parse_content_segments: extract `<think>` blocks
/// 2. parse_markdown_segments: extract fenced code blocks from text segments
/// 3. parse_math_segments: extract math expressions from non-code text
/// 4. highlight_code: syntax-highlight each code block
pub(super) fn build_cached_parse_result(content: &str, cx: &App) -> CachedParseResult {
    let content_segments = parse_content_segments(content);

    let cached_segments: Vec<CachedContentSegment> = content_segments
        .into_iter()
        .map(|segment| match segment {
            ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
            ContentSegment::Text(text) => {
                let markdown_segs = parse_markdown_segments(&text, false);
                let cached_md: Vec<CachedMarkdownSegment> = markdown_segs
                    .into_iter()
                    .map(|ms| match ms {
                        MarkdownSegment::CodeBlock { language, code }
                            if language.as_deref() == Some("mermaid") =>
                        {
                            let is_dark = cx.theme().mode.is_dark();
                            let svg_path = cx
                                .try_global::<MermaidRendererService>()
                                .and_then(|svc| svc.render_to_svg_file(&code, is_dark).ok());
                            CachedMarkdownSegment::MermaidDiagram {
                                source: code,
                                svg_path,
                            }
                        }
                        MarkdownSegment::CodeBlock { language, code } => {
                            let styles =
                                syntax_highlighter::highlight_code(&code, language.as_deref(), cx);
                            CachedMarkdownSegment::CodeBlock(CachedCodeBlock {
                                language,
                                code,
                                styles,
                            })
                        }
                        MarkdownSegment::Text(t) => {
                            let math_segs = parse_math_segments(&t);
                            CachedMarkdownSegment::TextWithMath(math_segs)
                        }
                        MarkdownSegment::IncompleteCodeBlock { .. } => {
                            unreachable!(
                                "IncompleteCodeBlock should not appear in non-streaming parse"
                            )
                        }
                        MarkdownSegment::UnclosedCodeBlock { language, code } => {
                            CachedMarkdownSegment::UnclosedCodeBlock { language, code }
                        }
                    })
                    .collect();
                CachedContentSegment::Text(cached_md)
            }
        })
        .collect();

    CachedParseResult {
        segments: cached_segments,
    }
}

/// Build a streaming parse result with incremental segment reuse.
///
/// During streaming, content only grows at the end. This function exploits that
/// property to avoid re-parsing stable content:
///
/// 1. Always run `parse_content_segments()` (cheap — just string::find for think tags)
/// 2. **Content segment level**: If segment count is unchanged and content only grew,
///    reuse all content segments except the last (they're stable).
/// 3. **Markdown segment level**: Within the last text segment, if md segment count
///    is unchanged, reuse all md segments except the last.
/// 4. Only the last (growing) markdown segment is re-parsed through math/highlighting.
///
/// When segment counts change (think block closed, code block completed), the
/// affected segment is fully re-parsed (one-time transition cost).
pub(super) fn build_streaming_parse_result(
    content: &str,
    prev: Option<&StreamingParseState>,
    cx: &App,
) -> StreamingParseState {
    let content_segments = parse_content_segments(content);
    let content_segment_count = content_segments.len();

    // Check if we can reuse the stable prefix from the previous render
    let can_reuse_prefix = prev.is_some_and(|p| {
        content.len() >= p.content_len && content_segment_count == p.content_segment_count
    });

    let cached_segments: Vec<CachedContentSegment> = if can_reuse_prefix {
        // SAFETY: can_reuse_prefix checks prev.is_some_and(...)
        let prev_state = prev.unwrap();
        let prev_segments = &prev_state.result.segments;
        let mut segments = Vec::with_capacity(content_segment_count);

        // Reuse all content segments except the last
        for seg in prev_segments
            .get(..prev_segments.len().saturating_sub(1))
            .unwrap_or(&[])
        {
            segments.push(seg.clone());
        }

        // Re-parse only the last content segment
        // SAFETY: content_segment_count > 0 (checked by can_reuse_prefix)
        let last = content_segments.into_iter().last().unwrap();
        segments.push(parse_content_segment_streaming(last, prev_state, cx));

        segments
    } else {
        // Full parse (first render or segment count changed)
        content_segments
            .into_iter()
            .map(|seg| parse_content_segment_streaming_fresh(seg, cx))
            .collect()
    };

    // Count md segments in last text segment (for next render's reuse check)
    let last_text_md_count = cached_segments
        .last()
        .map(|s| {
            if let CachedContentSegment::Text(mds) = s {
                mds.len()
            } else {
                0
            }
        })
        .unwrap_or(0);

    StreamingParseState {
        result: CachedParseResult {
            segments: cached_segments,
        },
        content_len: content.len(),
        content_segment_count,
        last_text_md_count,
    }
}

/// Parse the last content segment with markdown-level incremental reuse.
///
/// If the previous render had the same number of markdown segments within this
/// text block, reuse all but the last markdown segment (they're stable).
fn parse_content_segment_streaming(
    segment: ContentSegment,
    prev: &StreamingParseState,
    cx: &App,
) -> CachedContentSegment {
    match segment {
        ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
        ContentSegment::Text(text) => {
            let markdown_segs = parse_markdown_segments(&text, true);
            let md_count = markdown_segs.len();

            // Try markdown-level reuse: same count → reuse all but last
            let prev_md = prev
                .result
                .segments
                .last()
                .and_then(|s| {
                    if let CachedContentSegment::Text(mds) = s {
                        Some(mds)
                    } else {
                        None
                    }
                })
                .filter(|_| md_count == prev.last_text_md_count && md_count > 0);

            let cached_md = if let Some(prev_mds) = prev_md {
                let mut result = Vec::with_capacity(md_count);

                // Reuse all md segments except the last
                for seg in &prev_mds[..prev_mds.len().min(md_count - 1)] {
                    result.push(seg.clone());
                }

                // Parse only the last md segment
                // SAFETY: md_count > 0 (checked above)
                let last = markdown_segs.into_iter().last().unwrap();
                result.push(parse_markdown_segment_streaming(last, prev_mds, cx));

                result
            } else {
                // Full parse of all md segments (count changed or no prev).
                // Still pass prev_mds (if available) so that code blocks whose
                // language+code haven't changed can reuse their cached
                // highlight styles via try_reuse_code_block.
                let prev_mds_for_reuse: &[CachedMarkdownSegment] = prev
                    .result
                    .segments
                    .last()
                    .and_then(|s| {
                        if let CachedContentSegment::Text(mds) = s {
                            Some(mds.as_slice())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(&[]);
                markdown_segs
                    .into_iter()
                    .map(|ms| parse_markdown_segment_streaming(ms, prev_mds_for_reuse, cx))
                    .collect()
            };

            CachedContentSegment::Text(cached_md)
        }
    }
}

/// Parse a content segment without incremental reuse (first render or segment count changed).
fn parse_content_segment_streaming_fresh(
    segment: ContentSegment,
    cx: &App,
) -> CachedContentSegment {
    match segment {
        ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
        ContentSegment::Text(text) => {
            let markdown_segs = parse_markdown_segments(&text, true);
            let cached_md: Vec<CachedMarkdownSegment> = markdown_segs
                .into_iter()
                .map(|ms| parse_markdown_segment_streaming(ms, &[], cx))
                .collect();
            CachedContentSegment::Text(cached_md)
        }
    }
}

/// Convert a single `MarkdownSegment` into a `CachedMarkdownSegment`.
///
/// For complete code blocks, tries to reuse highlighting from `prev_mds`.
/// For incomplete code blocks, stores plain text (no highlighting).
fn parse_markdown_segment_streaming(
    segment: MarkdownSegment,
    prev_mds: &[CachedMarkdownSegment],
    cx: &App,
) -> CachedMarkdownSegment {
    match segment {
        MarkdownSegment::CodeBlock { language, code } if language.as_deref() == Some("mermaid") => {
            let is_dark = cx.theme().mode.is_dark();
            let svg_path = cx
                .try_global::<MermaidRendererService>()
                .and_then(|svc| svc.render_to_svg_file(&code, is_dark).ok());
            CachedMarkdownSegment::MermaidDiagram {
                source: code,
                svg_path,
            }
        }
        MarkdownSegment::CodeBlock { language, code } => {
            // Try to reuse highlighted spans from previous render
            if let Some(reused) = try_reuse_code_block(prev_mds, &language, &code) {
                CachedMarkdownSegment::CodeBlock(reused)
            } else {
                let styles = syntax_highlighter::highlight_code(&code, language.as_deref(), cx);
                CachedMarkdownSegment::CodeBlock(CachedCodeBlock {
                    language,
                    code,
                    styles,
                })
            }
        }
        MarkdownSegment::IncompleteCodeBlock { language, code } => {
            CachedMarkdownSegment::IncompleteCodeBlock { language, code }
        }
        MarkdownSegment::UnclosedCodeBlock { language, code } => {
            CachedMarkdownSegment::UnclosedCodeBlock { language, code }
        }
        MarkdownSegment::Text(t) => {
            let math_segs = parse_math_segments(&t);
            CachedMarkdownSegment::TextWithMath(math_segs)
        }
    }
}

/// Search previous markdown segments for a code block with matching
/// language and code content. Returns a clone of the `CachedCodeBlock`
/// (with its pre-computed highlight styles) if found.
fn try_reuse_code_block(
    prev_mds: &[CachedMarkdownSegment],
    language: &Option<String>,
    code: &str,
) -> Option<CachedCodeBlock> {
    for md_seg in prev_mds {
        if let CachedMarkdownSegment::CodeBlock(cb) = md_seg
            && &cb.language == language
            && cb.code == code
        {
            return Some(cb.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_markdown_segments ───────────────────────────────────────

    #[test]
    fn plain_text_returns_single_segment() {
        let segs = parse_markdown_segments("Hello world", false);
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], MarkdownSegment::Text(t) if t == "Hello world"));
    }

    #[test]
    fn code_block_is_extracted() {
        let input = "before\n```rust\nfn main() {}\n```\nafter";
        let segs = parse_markdown_segments(input, false);
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], MarkdownSegment::Text(_)));
        assert!(
            matches!(&segs[1], MarkdownSegment::CodeBlock { language, code }
                if language.as_deref() == Some("rust") && code == "fn main() {}")
        );
        assert!(matches!(&segs[2], MarkdownSegment::Text(t) if t.trim() == "after"));
    }

    #[test]
    fn incomplete_code_block_detected_in_streaming() {
        let input = "text\n```python\nprint('hi')";
        let segs = parse_markdown_segments(input, true);
        assert!(
            segs.iter()
                .any(|s| matches!(s, MarkdownSegment::IncompleteCodeBlock { .. }))
        );
    }

    #[test]
    fn unclosed_code_block_detected_when_not_streaming() {
        let input = "text\n```python\nprint('hi')";
        let segs = parse_markdown_segments(input, false);
        assert!(
            segs.iter()
                .any(|s| matches!(s, MarkdownSegment::UnclosedCodeBlock { .. }))
        );
        assert!(
            !segs
                .iter()
                .any(|s| matches!(s, MarkdownSegment::IncompleteCodeBlock { .. }))
        );
    }

    #[test]
    fn code_block_without_language() {
        let input = "```\nsome code\n```";
        let segs = parse_markdown_segments(input, false);
        assert_eq!(segs.len(), 1);
        assert!(
            matches!(&segs[0], MarkdownSegment::CodeBlock { language, .. } if language.is_none())
        );
    }

    #[test]
    fn multiple_code_blocks() {
        let input = "```js\nconsole.log(1)\n```\nmiddle\n```py\nprint(2)\n```";
        let segs = parse_markdown_segments(input, false);
        let code_blocks: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s, MarkdownSegment::CodeBlock { .. }))
            .collect();
        assert_eq!(code_blocks.len(), 2);
    }

    // ── detect_incomplete_code_block ──────────────────────────────────

    #[test]
    fn detect_incomplete_with_language() {
        let result = detect_incomplete_code_block("some text\n```rust\nlet x = 1;");
        assert!(result.is_some());
        let (_, lang, code) = result.unwrap();
        assert_eq!(lang.as_deref(), Some("rust"));
        assert_eq!(code, "let x = 1;");
    }

    #[test]
    fn detect_incomplete_without_newline_returns_none() {
        // No newline after opening fence — not a valid opening
        let result = detect_incomplete_code_block("```rust");
        assert!(result.is_none());
    }

    // ── parse_content_segments ────────────────────────────────────────

    #[test]
    fn plain_text_no_thinking() {
        let segs = parse_content_segments("Hello world");
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], ContentSegment::Text(t) if t == "Hello world"));
    }

    #[test]
    fn think_tag_extracted() {
        let input = "before<think>inner thought</think>after";
        let segs = parse_content_segments(input);
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], ContentSegment::Text(t) if t == "before"));
        assert!(matches!(&segs[1], ContentSegment::Thinking(t) if t == "inner thought"));
        assert!(matches!(&segs[2], ContentSegment::Text(t) if t == "after"));
    }

    #[test]
    fn thinking_tag_extracted() {
        let input = "<thinking>deep thought</thinking>result";
        let segs = parse_content_segments(input);
        assert_eq!(segs.len(), 2);
        assert!(matches!(&segs[0], ContentSegment::Thinking(t) if t == "deep thought"));
        assert!(matches!(&segs[1], ContentSegment::Text(t) if t == "result"));
    }

    #[test]
    fn thought_tag_extracted() {
        let input = "<thought>reasoning</thought>answer";
        let segs = parse_content_segments(input);
        assert_eq!(segs.len(), 2);
        assert!(matches!(&segs[0], ContentSegment::Thinking(t) if t == "reasoning"));
    }

    #[test]
    fn unclosed_think_tag_treated_as_incomplete() {
        let input = "before<think>streaming thought";
        let segs = parse_content_segments(input);
        assert_eq!(segs.len(), 2);
        assert!(matches!(&segs[0], ContentSegment::Text(t) if t == "before"));
        assert!(matches!(&segs[1], ContentSegment::Thinking(t) if t == "streaming thought"));
    }

    #[test]
    fn empty_thinking_block_skipped() {
        let input = "text<think></think>more";
        let segs = parse_content_segments(input);
        // Empty thinking block is skipped, so we get Text + Text
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| matches!(s, ContentSegment::Text(_))));
    }
}
