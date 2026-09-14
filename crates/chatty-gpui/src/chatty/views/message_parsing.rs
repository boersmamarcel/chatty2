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
use super::message_math_render::resolve_math_segments;
use super::parsed_cache::{
    CachedCodeBlock, CachedContentSegment, CachedMarkdownSegment, CachedParseResult, LiveTailCache,
    StreamingParseState,
};
use super::syntax_highlighter;
use std::sync::Arc;

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
/// Supports `<think>...</think>`, `<thinking>...</thinking>`, and
/// `<thought>...</thought>` patterns. Unclosed tags (common during streaming)
/// are treated as incomplete thinking blocks.
pub(super) fn parse_content_segments(content: &str) -> Vec<ContentSegment> {
    let mut segments = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        // Find the earliest opening tag among <thinking>, <thought>, <think>
        let find_thinking = remaining.find("<thinking>").map(|i| (i, 10usize));
        let find_thought = remaining.find("<thought>").map(|i| (i, 9usize));
        // <think> must not be the start of <thinking> (different prefix check isn't needed
        // since <thinking> starts with <think but is longer; find("<think>") won't match
        // inside "<thinking>" because the 8th char is 'i' not '>')
        let find_think = remaining.find("<think>").map(|i| (i, 7usize));

        let result = [find_thinking, find_thought, find_think]
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

        // Find the closing tag - support </think>, </thinking>, and </thought>
        let after_open = &remaining[start_idx + tag_len..];
        let end_tag_and_len = after_open
            .find("</think>")
            .map(|idx| (idx, 8)) // "</think>" is 8 chars
            .or_else(|| after_open.find("</thinking>").map(|idx| (idx, 11)))
            .or_else(|| after_open.find("</thought>").map(|idx| (idx, 10)));

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
/// 3. parse_math_segments + resolve_math_segments: extract math expressions
///    from non-code text and resolve each one's styled SVG path
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
                            let math_segs = resolve_math_segments(parse_math_segments(&t), cx);
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
    let can_reuse_prefix = content_segment_count > 0
        && prev.is_some_and(|p| {
            content.len() >= p.content_len && content_segment_count == p.content_segment_count
        });

    // The settled-prefix math parse survives any reuse decision above: it is
    // keyed by its own text, so a stale one is simply not matched.
    let mut live_tail = prev.and_then(|p| p.live_tail.clone());
    let mut last_text_md_count = 0;

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
        // SAFETY: can_reuse_prefix requires content_segment_count > 0
        let last = content_segments.into_iter().last().unwrap();
        segments.push(parse_content_segment_streaming(
            last,
            prev_state,
            &mut live_tail,
            &mut last_text_md_count,
            cx,
        ));

        segments
    } else {
        // Full parse (first render or segment count changed)
        let count = content_segments.len();
        content_segments
            .into_iter()
            .enumerate()
            .map(|(ix, seg)| {
                parse_content_segment_streaming_fresh(
                    seg,
                    ix + 1 == count,
                    &mut live_tail,
                    &mut last_text_md_count,
                    cx,
                )
            })
            .collect()
    };

    StreamingParseState {
        result: CachedParseResult {
            segments: cached_segments,
        },
        content_len: content.len(),
        content_segment_count,
        last_text_md_count,
        live_tail,
    }
}

/// Split the growing text segment into the settled prefix (through the last
/// newline) and the line still being streamed.
pub(super) fn split_live_tail(text: &str) -> (&str, &str) {
    match text.rfind('\n') {
        Some(ix) => text.split_at(ix + 1),
        None => ("", text),
    }
}

/// Parse the last markdown segment of a streaming message when it is text.
///
/// The settled prefix gets the full math parse with its SVG paths resolved
/// (AGE-394), reused from `cache` while the prefix is byte-identical; the open line is emitted as [`PlainTail`] and
/// never reaches the math parser, the markdown parser or Typst until a
/// newline promotes it (AGE-167).
///
/// [`PlainTail`]: CachedMarkdownSegment::PlainTail
fn parse_live_text_tail(
    text: &str,
    cache: &mut Option<LiveTailCache>,
    cx: &App,
) -> Vec<CachedMarkdownSegment> {
    let (settled_text, tail) = split_live_tail(text);
    let entry = match cache.take() {
        Some(prev) if prev.settled_text == settled_text => prev,
        _ => LiveTailCache {
            settled_text: settled_text.to_string(),
            settled: Arc::new(resolve_math_segments(parse_math_segments(settled_text), cx)),
        },
    };
    let mut out = Vec::with_capacity(2);
    if !entry.settled.is_empty() {
        out.push(CachedMarkdownSegment::TextWithMath(
            entry.settled.as_ref().clone(),
        ));
    }
    if !tail.is_empty() {
        out.push(CachedMarkdownSegment::PlainTail(tail.to_string()));
    }
    *cache = Some(entry);
    out
}

/// Convert the last markdown segment of the last text block: text becomes a
/// settled prefix plus a plain tail, everything else takes the ordinary path.
fn parse_last_markdown_segment_streaming(
    segment: MarkdownSegment,
    prev_mds: &[CachedMarkdownSegment],
    live_tail: &mut Option<LiveTailCache>,
    cx: &App,
) -> Vec<CachedMarkdownSegment> {
    match segment {
        MarkdownSegment::Text(t) => parse_live_text_tail(&t, live_tail, cx),
        other => vec![parse_markdown_segment_streaming(other, prev_mds, cx)],
    }
}

/// Parse the last content segment with markdown-level incremental reuse.
///
/// If the previous render had the same number of markdown segments within this
/// text block, reuse all but the last markdown segment (they're stable).
fn parse_content_segment_streaming(
    segment: ContentSegment,
    prev: &StreamingParseState,
    live_tail: &mut Option<LiveTailCache>,
    last_text_md_count: &mut usize,
    cx: &App,
) -> CachedContentSegment {
    match segment {
        ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
        ContentSegment::Text(text) => {
            let markdown_segs = parse_markdown_segments(&text, true);
            let md_count = markdown_segs.len();
            *last_text_md_count = md_count;

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
                let mut result = Vec::with_capacity(md_count + 1);

                // Reuse all md segments except the last. The cached list may
                // hold one more entry than `md_count` (the tail splits in two),
                // but its stable prefix is the first `md_count - 1` either way.
                for seg in &prev_mds[..prev_mds.len().min(md_count - 1)] {
                    result.push(seg.clone());
                }

                // Parse only the last md segment
                // SAFETY: md_count > 0 (checked above)
                let last = markdown_segs.into_iter().last().unwrap();
                result.extend(parse_last_markdown_segment_streaming(
                    last, prev_mds, live_tail, cx,
                ));

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
                let mut result = Vec::with_capacity(md_count + 1);
                let mut iter = markdown_segs.into_iter().peekable();
                while let Some(ms) = iter.next() {
                    if iter.peek().is_none() {
                        result.extend(parse_last_markdown_segment_streaming(
                            ms,
                            prev_mds_for_reuse,
                            live_tail,
                            cx,
                        ));
                    } else {
                        result.push(parse_markdown_segment_streaming(ms, prev_mds_for_reuse, cx));
                    }
                }
                result
            };

            CachedContentSegment::Text(cached_md)
        }
    }
}

/// Parse a content segment without incremental reuse (first render or segment count changed).
///
/// `is_last` marks the content segment that is still growing: only its final
/// markdown segment is split into a settled prefix and a plain tail.
fn parse_content_segment_streaming_fresh(
    segment: ContentSegment,
    is_last: bool,
    live_tail: &mut Option<LiveTailCache>,
    last_text_md_count: &mut usize,
    cx: &App,
) -> CachedContentSegment {
    match segment {
        ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
        ContentSegment::Text(text) => {
            let markdown_segs = parse_markdown_segments(&text, true);
            if is_last {
                *last_text_md_count = markdown_segs.len();
            }
            let mut cached_md: Vec<CachedMarkdownSegment> =
                Vec::with_capacity(markdown_segs.len() + 1);
            let mut iter = markdown_segs.into_iter().peekable();
            while let Some(ms) = iter.next() {
                if is_last && iter.peek().is_none() {
                    cached_md.extend(parse_last_markdown_segment_streaming(
                        ms,
                        &[],
                        live_tail,
                        cx,
                    ));
                } else {
                    cached_md.push(parse_markdown_segment_streaming(ms, &[], cx));
                }
            }
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
            let math_segs = resolve_math_segments(parse_math_segments(&t), cx);
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

    // ── live tail (AGE-167) ───────────────────────────────────────────

    use super::super::parsed_cache::CachedMathSegment;

    fn last_mds(state: &StreamingParseState) -> &[CachedMarkdownSegment] {
        match state.result.segments.last() {
            Some(CachedContentSegment::Text(mds)) => mds,
            other => panic!("expected a text segment, got {other:?}"),
        }
    }

    fn has_math(mds: &[CachedMarkdownSegment]) -> bool {
        mds.iter().any(|md| match md {
            CachedMarkdownSegment::TextWithMath(segs) => segs
                .iter()
                .any(|s| !matches!(s, CachedMathSegment::Text(_))),
            _ => false,
        })
    }

    #[test]
    fn split_live_tail_keeps_the_newline_on_the_settled_side() {
        assert_eq!(split_live_tail("no newline yet"), ("", "no newline yet"));
        assert_eq!(split_live_tail("a\nb\nc"), ("a\nb\n", "c"));
        assert_eq!(split_live_tail("a\n"), ("a\n", ""));
    }

    #[gpui::test]
    fn the_open_line_is_plain_and_the_settled_prefix_keeps_its_math(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // Resolving the settled prefix reads the theme (AGE-394).
            cx.set_global(gpui_component::Theme::default());
            let s1 = build_streaming_parse_result("Let $x$ be\nthe **bold", None, cx);
            let mds = last_mds(&s1);
            assert_eq!(mds.len(), 2, "{mds:?}");
            assert!(matches!(&mds[0], CachedMarkdownSegment::TextWithMath(segs)
                    if segs.iter().any(|s| matches!(s,
                        CachedMathSegment::InlineMath { latex, .. } if latex == "x"))));
            assert!(matches!(&mds[1], CachedMarkdownSegment::PlainTail(t) if t == "the **bold"));

            // Growing the open line does not re-parse the settled prefix.
            let s2 = build_streaming_parse_result("Let $x$ be\nthe **bold** one", Some(&s1), cx);
            let (Some(a), Some(b)) = (&s1.live_tail, &s2.live_tail) else {
                panic!("live tail cache missing");
            };
            assert!(
                Arc::ptr_eq(&a.settled, &b.settled),
                "settled prefix was re-parsed"
            );
            assert!(
                matches!(last_mds(&s2).last(), Some(CachedMarkdownSegment::PlainTail(t))
                    if t == "the **bold** one")
            );

            // A newline promotes the line: it now sits in the settled prefix.
            let s3 =
                build_streaming_parse_result("Let $x$ be\nthe **bold** one\nnext", Some(&s2), cx);
            assert!(!Arc::ptr_eq(
                &b.settled,
                &s3.live_tail.as_ref().unwrap().settled
            ));
            assert_eq!(
                s3.live_tail.as_ref().unwrap().settled_text,
                "Let $x$ be\nthe **bold** one\n"
            );
        });
    }

    #[gpui::test]
    fn an_open_math_fence_never_reaches_the_math_parser(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // Resolving the settled prefix reads the theme (AGE-394).
            cx.set_global(gpui_component::Theme::default());
            let mut prev: Option<StreamingParseState> = None;
            let mut text = String::from("Consider\n\n$$\n");
            for piece in ["E = ", "mc^2 + ", "$", "5 \\\\ ", "\\int_0^1 f"] {
                text.push_str(piece);
                let state = build_streaming_parse_result(&text, prev.as_ref(), cx);
                let mds = last_mds(&state);
                assert!(!has_math(mds), "open fence rendered math: {mds:?}");
                assert!(
                    matches!(mds.last(), Some(CachedMarkdownSegment::PlainTail(_))),
                    "{mds:?}"
                );
                prev = Some(state);
            }
            // Closing the fence promotes it to block math.
            text.push_str("\n$$\n");
            let state = build_streaming_parse_result(&text, prev.as_ref(), cx);
            assert!(has_math(last_mds(&state)), "{:?}", last_mds(&state));
        });
    }

    #[gpui::test]
    fn an_open_code_fence_is_not_highlighted(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // Resolving the settled prefix reads the theme (AGE-394).
            cx.set_global(gpui_component::Theme::default());
            let s1 = build_streaming_parse_result("Code:\n```rust\nfn main() {", None, cx);
            let mds = last_mds(&s1);
            assert!(
                matches!(mds.last(), Some(CachedMarkdownSegment::IncompleteCodeBlock { code, .. })
                    if code == "fn main() {"),
                "{mds:?}"
            );
            assert!(
                !mds.iter()
                    .any(|md| matches!(md, CachedMarkdownSegment::CodeBlock(_))),
            );
            // The text before the fence is settled, not a live tail.
            assert!(
                matches!(&mds[0], CachedMarkdownSegment::TextWithMath(_)),
                "{mds:?}"
            );
        });
    }

    #[gpui::test]
    fn stream_end_promotes_the_tail(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // Resolving the settled prefix reads the theme (AGE-394).
            cx.set_global(gpui_component::Theme::default());
            let streaming = build_streaming_parse_result("Sum $a+b$ done", None, cx);
            assert!(matches!(
                last_mds(&streaming).last(),
                Some(CachedMarkdownSegment::PlainTail(_))
            ));
            let done = build_cached_parse_result("Sum $a+b$ done", cx);
            let CachedContentSegment::Text(mds) = &done.segments[0] else {
                panic!()
            };
            assert!(has_math(mds), "{mds:?}");
            assert!(
                !mds.iter()
                    .any(|md| matches!(md, CachedMarkdownSegment::PlainTail(_)))
            );
        });
    }

    #[test]
    fn content_without_renderable_text_yields_no_segments() {
        // build_streaming_parse_result must not assume a non-empty segment list:
        // whitespace-only and empty-think-block content parse to zero segments.
        for input in ["", "   \n ", "<think></think>"] {
            assert!(parse_content_segments(input).is_empty(), "input: {input:?}");
        }
    }
}
