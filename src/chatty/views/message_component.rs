use crate::assets::CustomIcon;
use crate::chatty::models::MessageFeedback;
use crate::chatty::services::MathRendererService;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::text::TextView;
use gpui_component::{Icon, IconName, Sizable};
use std::path::PathBuf;
use tracing::{debug, warn};

use super::code_block_component::CodeBlockComponent;
use super::math_parser::{MathSegment, parse_math_segments};
use super::math_renderer::MathComponent;
use super::message_types::{AssistantMessage, SystemTrace};
use super::parsed_cache::{
    CachedCodeBlock, CachedContentSegment, CachedMarkdownSegment, CachedParseResult,
    ContentCacheKey, ParsedContentCache,
};
use super::syntax_highlighter;
use super::trace_components::SystemTraceView;

/// Message role indicator
#[derive(Clone, Debug)]
pub enum MessageRole {
    User,
    Assistant,
}

use lazy_static::lazy_static;
use regex::Regex;

/// Represents a segment of content - either text or a code block
#[derive(Clone, Debug)]
enum MarkdownSegment {
    Text(String),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
}

lazy_static! {
    // Regex to match fenced code blocks: ```language\ncode\n```
    static ref CODE_BLOCK_REGEX: Regex = Regex::new(
        r"(?s)```([a-zA-Z0-9_+-]*)
(.*?)
```"
    ).expect("CODE_BLOCK_REGEX pattern is valid");
}

/// Parse markdown content into segments of text and code blocks
fn parse_markdown_segments(content: &str) -> Vec<MarkdownSegment> {
    let mut segments = Vec::new();
    let mut last_end = 0;

    for cap in CODE_BLOCK_REGEX.captures_iter(content) {
        let match_start = cap.get(0).unwrap().start();
        let match_end = cap.get(0).unwrap().end();

        // Add text before this code block
        if match_start > last_end {
            let text = content[last_end..match_start].to_string();
            if !text.trim().is_empty() {
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

    // Add remaining text after last code block
    if last_end < content.len() {
        let text = content[last_end..].to_string();
        if !text.trim().is_empty() {
            segments.push(MarkdownSegment::Text(text));
        }
    }

    // If no segments were found, return the entire content as text
    if segments.is_empty() {
        segments.push(MarkdownSegment::Text(content.to_string()));
    }

    segments
}

/// Represents a parsed segment of message content
#[derive(Clone, Debug)]
enum ContentSegment {
    /// Regular text content (may contain markdown)
    Text(String),
    /// A thinking block with its content
    Thinking(String),
}

/// Display message structure used in chat view
#[derive(Clone)]
pub struct DisplayMessage {
    pub role: MessageRole,
    pub content: String,
    pub is_streaming: bool,
    pub system_trace_view: Option<Entity<SystemTraceView>>,
    // Track live trace during streaming
    pub live_trace: Option<SystemTrace>,
    // Track if this message should render as markdown
    pub is_markdown: bool,
    // File attachments (images/PDFs) for this message
    pub attachments: Vec<PathBuf>,
    // User feedback signal (thumbs up/down) for assistant messages
    pub feedback: Option<MessageFeedback>,
    // Index into the conversation's history (parallel arrays) for this message
    pub history_index: Option<usize>,
}

impl DisplayMessage {
    /// Create an assistant display message.
    ///
    /// Kept as a convenience constructor for future callers (e.g., tests or
    /// replay/export paths). Current production code builds `DisplayMessage`
    /// inline during stream processing.
    #[allow(dead_code)]
    pub fn from_assistant_message(assistant_msg: &AssistantMessage, cx: &mut App) -> Self {
        // Only create a trace view if the trace exists AND has items
        let trace_view = assistant_msg
            .system_trace
            .as_ref()
            .filter(|trace| trace.has_items())
            .map(|trace| cx.new(|_cx| SystemTraceView::new(trace.clone())));

        Self {
            role: MessageRole::Assistant,
            content: assistant_msg.text.clone(),
            is_streaming: assistant_msg.is_streaming,
            system_trace_view: trace_view,
            live_trace: None,
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        }
    }
}

/// Wrapper component for rendering markdown content
#[derive(IntoElement, Clone)]
struct MarkdownContent {
    content: String,
    message_index: usize,
}

impl RenderOnce for MarkdownContent {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Use message index for stable ID during streaming
        let id = ElementId::Name(format!("msg-{}-markdown", self.message_index).into());

        TextView::markdown(id, self.content, window, cx).selectable(true)
    }
}

/// Render pre-parsed math segments to GPUI elements.
///
/// Accepts `&[MathSegment]` so it can be used both from the live parsing path
/// (`render_math_aware_content`) and from the cached path (`render_from_cached`).
fn render_math_segments(
    math_segments: &[MathSegment],
    base_index: usize,
    cx: &App,
) -> Vec<AnyElement> {
    let mut elements = Vec::new();
    let mut inline_row: Vec<AnyElement> = Vec::new();
    for (seg_idx, segment) in math_segments.iter().enumerate() {
        let element_index = base_index * 1000 + seg_idx;

        match segment {
            MathSegment::Text(text) => {
                // Add text to inline row
                inline_row.push(
                    MarkdownContent {
                        content: text.clone(),
                        message_index: element_index,
                    }
                    .into_any_element(),
                );
            }
            MathSegment::InlineMath(math_content) => {
                // Add inline math to inline row
                let element_id = ElementId::Name(format!("math-inline-{}", element_index).into());

                // Pre-compute styled SVG path with theme color
                let math_elem = if let Some(service) = cx.try_global::<MathRendererService>() {
                    let theme_color = cx.theme().foreground;
                    match service.render_to_styled_svg_file(math_content, true, theme_color) {
                        Ok(svg_path) => MathComponent::with_svg_path(
                            math_content.clone(),
                            true,
                            element_id,
                            svg_path,
                        ),
                        Err(e) => {
                            warn!(error = ?e, content = %math_content, is_inline = true, "Failed to pre-render inline math");
                            MathComponent::new(math_content.clone(), true, element_id)
                        }
                    }
                } else {
                    warn!(content = %math_content, "Math renderer service unavailable for inline math");
                    MathComponent::new(math_content.clone(), true, element_id)
                };

                inline_row.push(math_elem.into_any_element());
            }
            MathSegment::BlockMath(math_content) => {
                // Flush any pending inline content first
                if !inline_row.is_empty() {
                    elements.push(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .children(inline_row.drain(..))
                            .into_any_element(),
                    );
                }

                // Render block math as its own element
                let element_id = ElementId::Name(format!("math-block-{}", element_index).into());

                // Pre-compute styled SVG path with theme color
                let math_elem = if let Some(service) = cx.try_global::<MathRendererService>() {
                    let theme_color = cx.theme().foreground;
                    match service.render_to_styled_svg_file(math_content, false, theme_color) {
                        Ok(svg_path) => MathComponent::with_svg_path(
                            math_content.clone(),
                            false,
                            element_id,
                            svg_path,
                        ),
                        Err(e) => {
                            warn!(error = ?e, content = %math_content, is_inline = false, "Failed to pre-render block math");
                            MathComponent::new(math_content.clone(), false, element_id)
                        }
                    }
                } else {
                    warn!(content = %math_content, "Math renderer service unavailable for block math");
                    MathComponent::new(math_content.clone(), false, element_id)
                };

                elements.push(math_elem.into_any_element());
            }
        }
    }

    // Flush any remaining inline content
    if !inline_row.is_empty() {
        elements.push(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .children(inline_row)
                .into_any_element(),
        );
    }

    elements
}

/// Run the full three-stage parsing pipeline and return a cacheable result.
///
/// Phases:
/// 1. parse_content_segments: extract `<think>` blocks
/// 2. parse_markdown_segments: extract fenced code blocks from text segments
/// 3. parse_math_segments: extract math expressions from non-code text
/// 4. highlight_code: syntax-highlight each code block
fn build_cached_parse_result(content: &str, cx: &App) -> CachedParseResult {
    let content_segments = parse_content_segments(content);

    let cached_segments: Vec<CachedContentSegment> = content_segments
        .into_iter()
        .map(|segment| match segment {
            ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
            ContentSegment::Text(text) => {
                let markdown_segs = parse_markdown_segments(&text);
                let cached_md: Vec<CachedMarkdownSegment> = markdown_segs
                    .into_iter()
                    .map(|ms| match ms {
                        MarkdownSegment::CodeBlock { language, code } => {
                            let spans =
                                syntax_highlighter::highlight_code(&code, language.as_deref(), cx);
                            CachedMarkdownSegment::CodeBlock(CachedCodeBlock {
                                language,
                                code,
                                highlighted_spans: spans,
                            })
                        }
                        MarkdownSegment::Text(t) => {
                            let math_segs = parse_math_segments(&t);
                            CachedMarkdownSegment::TextWithMath(math_segs)
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

/// Build a CachedParseResult for streaming content, reusing syntax highlighting
/// from the previous result for code blocks that haven't changed.
///
/// Completed code blocks (those with a closing ```) will not change as more text
/// streams in, so their expensive `highlight_code()` results can be safely reused.
fn build_streaming_parse_result(
    content: &str,
    prev: Option<&CachedParseResult>,
    cx: &App,
) -> CachedParseResult {
    let content_segments = parse_content_segments(content);

    let cached_segments: Vec<CachedContentSegment> = content_segments
        .into_iter()
        .map(|segment| match segment {
            ContentSegment::Thinking(text) => CachedContentSegment::Thinking(text),
            ContentSegment::Text(text) => {
                let markdown_segs = parse_markdown_segments(&text);
                let cached_md: Vec<CachedMarkdownSegment> = markdown_segs
                    .into_iter()
                    .map(|ms| match ms {
                        MarkdownSegment::CodeBlock { language, code } => {
                            // Try to reuse highlighting from the previous render
                            if let Some(reused) = try_reuse_code_block(prev, &language, &code) {
                                CachedMarkdownSegment::CodeBlock(reused)
                            } else {
                                let spans = syntax_highlighter::highlight_code(
                                    &code,
                                    language.as_deref(),
                                    cx,
                                );
                                CachedMarkdownSegment::CodeBlock(CachedCodeBlock {
                                    language,
                                    code,
                                    highlighted_spans: spans,
                                })
                            }
                        }
                        MarkdownSegment::Text(t) => {
                            let math_segs = parse_math_segments(&t);
                            CachedMarkdownSegment::TextWithMath(math_segs)
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

/// Search a previous `CachedParseResult` for a code block with matching
/// language and code content. Returns a clone of the `CachedCodeBlock`
/// (with its pre-computed highlighted spans) if found.
fn try_reuse_code_block(
    prev: Option<&CachedParseResult>,
    language: &Option<String>,
    code: &str,
) -> Option<CachedCodeBlock> {
    let prev = prev?;
    for segment in &prev.segments {
        if let CachedContentSegment::Text(md_segments) = segment {
            for md_seg in md_segments {
                if let CachedMarkdownSegment::CodeBlock(cb) = md_seg
                    && &cb.language == language
                    && cb.code == code
                {
                    return Some(cb.clone());
                }
            }
        }
    }
    None
}

/// Build GPUI elements from pre-parsed cached content.
///
/// Mirrors the logic of the thinking-block + code-block + math rendering paths
/// but reads from the cache instead of re-parsing.
fn render_from_cached(cached: &CachedParseResult, index: usize, cx: &App) -> Vec<AnyElement> {
    cached
        .segments
        .iter()
        .enumerate()
        .flat_map(|(seg_idx, segment)| match segment {
            CachedContentSegment::Thinking(content) => {
                vec![render_thinking_block(content, index, seg_idx, cx).into_any_element()]
            }
            CachedContentSegment::Text(md_segments) => {
                render_cached_markdown_segments(md_segments, index * 100 + seg_idx, cx)
            }
        })
        .collect()
}

/// Render cached markdown segments (code blocks with pre-highlighted spans,
/// text with pre-parsed math segments).
fn render_cached_markdown_segments(
    segments: &[CachedMarkdownSegment],
    base_index: usize,
    cx: &App,
) -> Vec<AnyElement> {
    let mut elements = Vec::new();
    let mut code_block_index = 0;

    for segment in segments {
        match segment {
            CachedMarkdownSegment::CodeBlock(cached_cb) => {
                let block = CodeBlockComponent::with_highlighted_spans(
                    cached_cb.language.clone(),
                    cached_cb.code.clone(),
                    cached_cb.highlighted_spans.clone(),
                    base_index * 100 + code_block_index,
                );
                elements.push(block.into_any_element());
                code_block_index += 1;
            }
            CachedMarkdownSegment::TextWithMath(math_segments) => {
                let math_elements = render_math_segments(math_segments, base_index, cx);
                elements.extend(math_elements);
            }
        }
    }

    elements
}

/// Parse content to extract thinking blocks and regular text segments
/// Supports <think>...</think>, <thinking>...</thinking>, and <thought>...</thought> patterns
fn parse_content_segments(content: &str) -> Vec<ContentSegment> {
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

/// Render a thinking block with special styling
fn render_thinking_block(
    content: &str,
    index: usize,
    segment_index: usize,
    cx: &App,
) -> Stateful<Div> {
    let border_color = cx.theme().border;
    let muted_text = cx.theme().muted_foreground;
    let bg_color = cx.theme().muted;

    div()
        .id(ElementId::Name(
            format!("msg-{}-thinking-{}", index, segment_index).into(),
        ))
        .mb_3()
        .p_3()
        .bg(bg_color)
        .border_l_4()
        .border_color(border_color)
        .rounded_md()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .mb_2()
                .child(Icon::new(CustomIcon::Brain).size_3().text_color(muted_text))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted_text)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Thinking"),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(muted_text)
                .child(content.to_string()),
        )
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];

fn is_image_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Render attachment images as thumbnails above the message text
fn render_attachments(attachments: &[PathBuf], id_prefix: &str, cx: &App) -> Div {
    let border_color = cx.theme().border;

    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .mb_2()
        .children(attachments.iter().enumerate().map(|(i, path)| {
            let element_id = ElementId::Name(format!("{}-att-{}", id_prefix, i).into());

            if is_image_file(path) {
                // Render image thumbnail
                div()
                    .id(element_id)
                    .rounded_md()
                    .border_1()
                    .border_color(border_color)
                    .overflow_hidden()
                    .child(
                        img(path.clone())
                            .max_w(px(300.))
                            .max_h(px(300.))
                            .rounded_md(),
                    )
            } else {
                // Non-image attachment (PDF etc) - show filename
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();

                div()
                    .id(element_id)
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border_color)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(CustomIcon::Paperclip).size_4())
                    .child(div().text_sm().child(filename))
            }
        }))
}

/// Extract the file path from an `add_attachment` tool call output JSON.
/// Returns `None` if the output is missing, not valid JSON, or lacks a `"path"` field.
fn extract_attachment_path(tool_call: &super::message_types::ToolCallBlock) -> Option<PathBuf> {
    let output = tool_call.output.as_ref()?;
    let json: serde_json::Value = serde_json::from_str(output).ok()?;
    let path_str = json.get("path")?.as_str()?;
    Some(PathBuf::from(path_str))
}

/// Render a text segment using the cache, handling embedded `<thinking>` blocks.
///
/// For finalized markdown content, uses the persistent cache to avoid re-parsing.
/// For streaming markdown, uses `build_streaming_parse_result` which reuses
/// code block highlighting from the previous render to avoid O(n²) re-highlighting.
#[allow(clippy::too_many_arguments)]
fn render_text_segment_cached(
    text_segment: &str,
    base_index: usize,
    is_markdown: bool,
    is_streaming: bool,
    is_dark: bool,
    parsed_cache: &mut ParsedContentCache,
    streaming_cache: &mut Option<CachedParseResult>,
    cx: &App,
) -> Vec<AnyElement> {
    if is_markdown && !is_streaming {
        // Finalized: use cache to avoid re-parsing on every render
        let cache_key = ContentCacheKey::new(text_segment, is_dark);
        if parsed_cache.get(&cache_key).is_none() {
            let result = build_cached_parse_result(text_segment, cx);
            parsed_cache.insert(cache_key, result);
        }
        let cached = parsed_cache.get(&cache_key).unwrap();
        render_from_cached(cached, base_index, cx)
    } else if is_markdown {
        // Streaming: reuse code block highlights from previous render
        let result = build_streaming_parse_result(text_segment, streaming_cache.as_ref(), cx);
        let elements = render_from_cached(&result, base_index, cx);
        *streaming_cache = Some(result);
        elements
    } else {
        vec![div().child(text_segment.to_string()).into_any_element()]
    }
}

/// Render interleaved content: text segments mixed with tool calls
#[allow(clippy::too_many_arguments)]
fn render_interleaved_content<F>(
    msg: &DisplayMessage,
    index: usize,
    mut container: Div,
    collapsed_tool_calls: &std::collections::HashMap<(usize, usize), bool>,
    parsed_cache: &mut ParsedContentCache,
    streaming_cache: &mut Option<CachedParseResult>,
    on_toggle_tool: F,
    cx: &App,
) -> Div
where
    F: Fn(usize, usize, &mut App) + 'static + Clone,
{
    use super::message_types::TraceItem;

    let is_dark = cx.theme().mode.is_dark();

    // Get the trace items from the trace view
    let trace_items = msg
        .system_trace_view
        .as_ref()
        .map(|view_entity| view_entity.read(cx).get_trace().items.clone())
        .unwrap_or_default();

    if trace_items.is_empty() {
        // No tool calls, but still handle any thinking blocks in the content
        let elements = render_text_segment_cached(
            &msg.content,
            index,
            msg.is_markdown,
            msg.is_streaming,
            is_dark,
            parsed_cache,
            streaming_cache,
            cx,
        );
        return container.children(elements);
    }

    // Track position in message content
    let mut last_text_end = 0;
    let full_content = &msg.content;

    for (tool_idx, item) in trace_items.iter().enumerate() {
        if let TraceItem::ToolCall(tool_call) = item {
            // Render text that came before this tool call
            let text_before = &tool_call.text_before;

            debug!(
                tool_idx = tool_idx,
                tool_name = %tool_call.tool_name,
                text_before_len = text_before.len(),
                last_text_end = last_text_end,
                condition = text_before.len() > last_text_end,
                "Processing tool call for interleaving"
            );

            // Only render if there's new text since the last segment
            if text_before.len() > last_text_end {
                let text_segment = &text_before[last_text_end..];
                if !text_segment.is_empty() {
                    let elements = render_text_segment_cached(
                        text_segment,
                        index * 100 + tool_idx,
                        msg.is_markdown,
                        msg.is_streaming,
                        is_dark,
                        parsed_cache,
                        streaming_cache,
                        cx,
                    );
                    container = container.children(elements);
                }
                last_text_end = text_before.len();
            }

            // Render the tool call using trace_components
            let is_collapsed = collapsed_tool_calls
                .get(&(index, tool_idx))
                .copied()
                .unwrap_or(true); // Default to collapsed

            let on_toggle_clone = on_toggle_tool.clone();
            let msg_idx = index;
            let toggle_callback =
                move |_event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                    on_toggle_clone(msg_idx, tool_idx, cx);
                };

            container = container.child(div().mt_2().mb_2().child(
                super::trace_components::render_tool_call_inline(
                    tool_call,
                    index,
                    tool_idx,
                    is_collapsed,
                    toggle_callback,
                    cx,
                ),
            ));

            // If this is a successful add_attachment call, render the image/PDF inline
            if tool_call.tool_name == "add_attachment"
                && matches!(
                    tool_call.state,
                    super::message_types::ToolCallState::Success
                )
                && let Some(path) = extract_attachment_path(tool_call)
            {
                container = container.child(render_attachments(
                    &[path],
                    &format!("msg-{index}-tool-{tool_idx}"),
                    cx,
                ));
            }
        }
    }

    // Render any remaining text after the last tool call
    if last_text_end < full_content.len() {
        let remaining_text = &full_content[last_text_end..];
        if !remaining_text.is_empty() {
            let elements = render_text_segment_cached(
                remaining_text,
                index * 1000,
                msg.is_markdown,
                msg.is_streaming,
                is_dark,
                parsed_cache,
                streaming_cache,
                cx,
            );
            container = container.children(elements);
        }
    }

    container
}

/// Render the action row (copy + feedback + regenerate buttons) for assistant messages
fn render_assistant_actions<G, R>(
    content: &str,
    feedback: &Option<MessageFeedback>,
    index: usize,
    is_last_message: bool,
    on_feedback: G,
    on_regenerate: R,
    cx: &App,
) -> Div
where
    G: Fn(usize, Option<MessageFeedback>, &mut App) + 'static + Clone,
    R: Fn(usize, &mut App) + 'static + Clone,
{
    let muted = cx.theme().muted_foreground;

    let thumbs_up_active = matches!(feedback, Some(MessageFeedback::ThumbsUp));
    let thumbs_down_active = matches!(feedback, Some(MessageFeedback::ThumbsDown));

    div()
        .flex()
        .justify_end()
        .gap_1()
        .pt_2()
        .child(
            Button::new(ElementId::Name(format!("thumbs-up-msg-{}", index).into()))
                .ghost()
                .xsmall()
                .icon(
                    Icon::new(IconName::ThumbsUp).text_color(if thumbs_up_active {
                        gpui_component::green_500()
                    } else {
                        muted
                    }),
                )
                .tooltip("Good response")
                .on_click({
                    let on_feedback = on_feedback.clone();
                    let new_feedback = if thumbs_up_active {
                        None
                    } else {
                        Some(MessageFeedback::ThumbsUp)
                    };
                    move |_event, _window, cx| {
                        on_feedback(index, new_feedback.clone(), cx);
                    }
                }),
        )
        .child(
            Button::new(ElementId::Name(format!("thumbs-down-msg-{}", index).into()))
                .ghost()
                .xsmall()
                .icon(
                    Icon::new(IconName::ThumbsDown).text_color(if thumbs_down_active {
                        gpui_component::red_500()
                    } else {
                        muted
                    }),
                )
                .tooltip("Bad response")
                .on_click({
                    let on_feedback = on_feedback.clone();
                    let new_feedback = if thumbs_down_active {
                        None
                    } else {
                        Some(MessageFeedback::ThumbsDown)
                    };
                    move |_event, _window, cx| {
                        on_feedback(index, new_feedback.clone(), cx);
                    }
                }),
        )
        .when(is_last_message, |this| {
            this.child(
                Button::new(ElementId::Name(format!("regenerate-msg-{}", index).into()))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(CustomIcon::Refresh).text_color(muted))
                    .tooltip("Regenerate response")
                    .on_click({
                        let on_regenerate = on_regenerate.clone();
                        move |_event, _window, cx| {
                            on_regenerate(index, cx);
                        }
                    }),
            )
        })
        .child(
            Button::new(ElementId::Name(format!("copy-msg-{}", index).into()))
                .ghost()
                .xsmall()
                .icon(Icon::new(CustomIcon::Copy))
                .tooltip("Copy message")
                .on_click({
                    let content = content.to_string();
                    move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(content.clone()));
                    }
                }),
        )
}

#[allow(clippy::too_many_arguments)]
pub fn render_message<F, G, R>(
    msg: &DisplayMessage,
    index: usize,
    is_last_message: bool,
    collapsed_tool_calls: &std::collections::HashMap<(usize, usize), bool>,
    parsed_cache: &mut ParsedContentCache,
    streaming_cache: &mut Option<CachedParseResult>,
    on_toggle_tool: F,
    on_feedback: G,
    on_regenerate: R,
    cx: &App,
) -> AnyElement
where
    F: Fn(usize, usize, &mut App) + 'static + Clone,
    G: Fn(usize, Option<MessageFeedback>, &mut App) + 'static + Clone,
    R: Fn(usize, &mut App) + 'static + Clone,
{
    let is_dark = cx.theme().mode.is_dark();

    // Full render for messages in viewport
    let mut container = div()
        .max_w(relative(1.)) // Max 100% of container width
        .p_3()
        .rounded_lg();

    // Only apply background color to user messages
    // Assistant/system messages use the main background (no additional background)
    container = match msg.role {
        MessageRole::User => container.bg(cx.theme().secondary),
        MessageRole::Assistant => container, // No background, uses main bg
    };

    // Check if we should render interleaved content (tool calls mixed with text)
    let should_interleave =
        matches!(msg.role, MessageRole::Assistant) && msg.system_trace_view.is_some();

    // Render attachments (images/PDFs) if present.
    // For assistant messages with interleaved tool calls, attachments are rendered
    // inline after the `add_attachment` tool call that produced them (see render_interleaved_content).
    if !msg.attachments.is_empty() && !should_interleave {
        container = container.child(render_attachments(
            &msg.attachments,
            &format!("msg-{index}"),
            cx,
        ));
    }

    // For non-interleaved assistant messages with markdown, use optimized render paths:
    // - Finalized: cache the parse result to avoid re-parsing on every render
    // - Streaming: reuse code block highlights from the previous render
    if matches!(msg.role, MessageRole::Assistant) && !should_interleave && msg.is_markdown {
        let children = if !msg.is_streaming {
            // Finalized: use cached parse result
            let cache_key = ContentCacheKey::new(&msg.content, is_dark);
            if parsed_cache.get(&cache_key).is_none() {
                let result = build_cached_parse_result(&msg.content, cx);
                parsed_cache.insert(cache_key, result);
            }
            let cached = parsed_cache.get(&cache_key).unwrap();
            render_from_cached(cached, index, cx)
        } else {
            // Streaming: reuse code block highlights from previous render
            let result = build_streaming_parse_result(&msg.content, streaming_cache.as_ref(), cx);
            let elements = render_from_cached(&result, index, cx);
            *streaming_cache = Some(result);
            elements
        };

        let message_with_content = container.children(children);

        // Wrap with action buttons for finalized assistant messages
        let is_finalized = !msg.is_streaming && msg.live_trace.is_none();
        return match msg.role {
            MessageRole::Assistant if is_finalized && !msg.content.is_empty() => div()
                .child(message_with_content)
                .child(render_assistant_actions(
                    &msg.content,
                    &msg.feedback,
                    index,
                    is_last_message,
                    on_feedback,
                    on_regenerate,
                    cx,
                ))
                .into_any_element(),
            _ => message_with_content.into_any_element(),
        };
    }

    // Render content based on whether it's markdown (no thinking blocks found)
    // Markdown is rendered through the full pipeline for both streaming and finalized;
    // finalized content is cached, streaming content is parsed fresh each render.

    debug!(
        index = index,
        is_markdown = msg.is_markdown,
        is_streaming = msg.is_streaming,
        content_len = msg.content.len(),
        should_interleave = should_interleave,
        "render_message: deciding markdown path"
    );

    let final_container = if should_interleave {
        // Render interleaved content (text mixed with tool calls)
        render_interleaved_content(
            msg,
            index,
            container,
            collapsed_tool_calls,
            parsed_cache,
            streaming_cache,
            on_toggle_tool,
            cx,
        )
    } else if msg.is_markdown {
        // Markdown content — use cache for finalized, streaming parse for streaming
        let content_elements = render_text_segment_cached(
            &msg.content,
            index,
            msg.is_markdown,
            msg.is_streaming,
            is_dark,
            parsed_cache,
            streaming_cache,
            cx,
        );
        container.children(content_elements)
    } else {
        // Non-markdown plain text
        container.child(msg.content.clone())
    };

    // Wrap with action buttons for finalized assistant messages
    // (hide feedback row while still streaming or content is empty)
    let is_finalized = !msg.is_streaming && msg.live_trace.is_none();
    match msg.role {
        MessageRole::Assistant if is_finalized && !msg.content.is_empty() => div()
            .child(final_container)
            .child(render_assistant_actions(
                &msg.content,
                &msg.feedback,
                index,
                is_last_message,
                on_feedback,
                on_regenerate,
                cx,
            ))
            .into_any_element(),
        _ => final_container.into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    // Re-import standard #[test] to shadow gpui::test from `use gpui::*`
    use core::prelude::rust_2021::test;

    use super::*;
    use std::time::Duration;

    /// Helper to build a ToolCallBlock with the given output.
    fn make_tool_call(output: Option<&str>) -> super::super::message_types::ToolCallBlock {
        super::super::message_types::ToolCallBlock {
            id: "test-id".to_string(),
            tool_name: "add_attachment".to_string(),
            display_name: "add_attachment".to_string(),
            input: "{}".to_string(),
            output: output.map(|s| s.to_string()),
            output_preview: None,
            state: super::super::message_types::ToolCallState::Success,
            duration: Some(Duration::from_millis(100)),
            text_before: String::new(),
        }
    }

    #[test]
    fn extract_attachment_path_valid_json() {
        let tc = make_tool_call(Some(
            r#"{"path": "/tmp/output/chart.png", "file_type": "image", "message": "ok"}"#,
        ));
        assert_eq!(
            extract_attachment_path(&tc),
            Some(PathBuf::from("/tmp/output/chart.png"))
        );
    }

    #[test]
    fn extract_attachment_path_no_output() {
        let tc = make_tool_call(None);
        assert_eq!(extract_attachment_path(&tc), None);
    }

    #[test]
    fn extract_attachment_path_invalid_json() {
        let tc = make_tool_call(Some("not json at all"));
        assert_eq!(extract_attachment_path(&tc), None);
    }

    #[test]
    fn extract_attachment_path_missing_path_field() {
        let tc = make_tool_call(Some(r#"{"file_type": "image", "message": "ok"}"#));
        assert_eq!(extract_attachment_path(&tc), None);
    }

    #[test]
    fn extract_attachment_path_path_not_string() {
        let tc = make_tool_call(Some(r#"{"path": 42}"#));
        assert_eq!(extract_attachment_path(&tc), None);
    }

    #[test]
    fn extract_attachment_path_empty_json_object() {
        let tc = make_tool_call(Some("{}"));
        assert_eq!(extract_attachment_path(&tc), None);
    }
}
