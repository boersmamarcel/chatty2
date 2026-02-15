use crate::assets::CustomIcon;
use crate::chatty::services::MathRendererService;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::text::TextView;
use gpui_component::{Icon, Sizable};
use std::path::PathBuf;
use tracing::{debug, warn};

use super::code_block_component::CodeBlockComponent;
use super::math_parser::{MathSegment, parse_math_segments};
use super::math_renderer::MathComponent;
use super::message_types::{AssistantMessage, SystemTrace};
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
}

impl DisplayMessage {
    /// Create an assistant display message
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

/// Render content with math awareness
///
/// This function parses content for math expressions and renders them appropriately.
/// Math expressions are rendered using MathComponent while regular text uses MarkdownContent.
fn render_math_aware_content(content: &str, base_index: usize, cx: &App) -> Vec<AnyElement> {
    // Parse for math segments
    let math_segments = parse_math_segments(content);

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

/// Render content with code block awareness, then math awareness
///
/// This function:
/// 1. First parses for fenced code blocks (```)
/// 2. Renders code blocks with copy buttons
/// 3. For text segments, parses for math expressions
fn render_content_with_code_blocks(content: &str, base_index: usize, cx: &App) -> Vec<AnyElement> {
    let markdown_segments = parse_markdown_segments(content);
    let mut elements = Vec::new();
    let mut code_block_index = 0;

    for segment in markdown_segments {
        match segment {
            MarkdownSegment::CodeBlock { language, code } => {
                // Render code block with copy button
                let code_block =
                    CodeBlockComponent::new(language, code, base_index * 100 + code_block_index);
                elements.push(code_block.into_any_element());
                code_block_index += 1;
            }
            MarkdownSegment::Text(text) => {
                // Parse text for math expressions
                let math_elements = render_math_aware_content(&text, base_index, cx);
                elements.extend(math_elements);
            }
        }
    }

    elements
}

/// Parse content to extract thinking blocks and regular text segments
/// Supports <think>...</think> and <thinking>...</thinking> patterns
fn parse_content_segments(content: &str) -> Vec<ContentSegment> {
    let mut segments = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        // Look for opening tag - support both <think> and <thinking>
        let (start_idx, tag_len) = if let Some(idx) = remaining.find("<think>") {
            // Check it's not actually <thinking>
            if remaining[idx..].starts_with("<thinking>") {
                (idx, 10) // "<thinking>" is 10 chars
            } else {
                (idx, 7) // "<think>" is 7 chars
            }
        } else if let Some(idx) = remaining.find("<thinking>") {
            (idx, 10)
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

        // Find the closing tag - support </think> and </thinking>
        let after_open = &remaining[start_idx + tag_len..];
        let end_tag_and_len = after_open
            .find("</think>")
            .map(|idx| (idx, 8)) // "</think>" is 8 chars
            .or_else(|| after_open.find("</thinking>").map(|idx| (idx, 11)));

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
            div().flex().items_center().gap_2().mb_2().child(
                div()
                    .text_xs()
                    .text_color(muted_text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("💭 Thinking"),
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
fn render_attachments(attachments: &[PathBuf], index: usize, cx: &App) -> Div {
    let border_color = cx.theme().border;

    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .mb_2()
        .children(attachments.iter().enumerate().map(|(i, path)| {
            let element_id = ElementId::Name(format!("msg-{}-attachment-{}", index, i).into());

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
                    .text_sm()
                    .child(format!("📎 {}", filename))
            }
        }))
}

/// Render interleaved content: text segments mixed with tool calls
fn render_interleaved_content<F>(
    msg: &DisplayMessage,
    index: usize,
    mut container: Div,
    collapsed_tool_calls: &std::collections::HashMap<(usize, usize), bool>,
    on_toggle_tool: F,
    cx: &App,
) -> Div
where
    F: Fn(usize, usize, &mut App) + 'static + Clone,
{
    use super::message_types::TraceItem;

    // Get the trace items from the trace view
    let trace_items = msg
        .system_trace_view
        .as_ref()
        .map(|view_entity| view_entity.read(cx).get_trace().items.clone())
        .unwrap_or_default();

    if trace_items.is_empty() {
        // No tool calls, just render content normally
        if msg.is_markdown && !msg.is_streaming {
            let content_elements = render_content_with_code_blocks(&msg.content, index, cx);
            return container.children(content_elements);
        } else {
            return container.child(msg.content.clone());
        }
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
                    if msg.is_markdown && !msg.is_streaming {
                        let text_elements = render_content_with_code_blocks(
                            text_segment,
                            index * 100 + tool_idx,
                            cx,
                        );
                        container = container.children(text_elements);
                    } else {
                        container = container.child(div().child(text_segment.to_string()));
                    }
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
        }
    }

    // Render any remaining text after the last tool call
    if last_text_end < full_content.len() {
        let remaining_text = &full_content[last_text_end..];
        if !remaining_text.is_empty() {
            if msg.is_markdown && !msg.is_streaming {
                let text_elements =
                    render_content_with_code_blocks(remaining_text, index * 1000, cx);
                container = container.children(text_elements);
            } else {
                container = container.child(div().child(remaining_text.to_string()));
            }
        }
    }

    container
}

pub fn render_message<F>(
    msg: &DisplayMessage,
    index: usize,
    collapsed_tool_calls: &std::collections::HashMap<(usize, usize), bool>,
    on_toggle_tool: F,
    cx: &App,
) -> AnyElement
where
    F: Fn(usize, usize, &mut App) + 'static + Clone,
{
    // If not in viewport window, render lightweight placeholder
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

    // Render attachments (images/PDFs) if present
    if !msg.attachments.is_empty() {
        container = container.child(render_attachments(&msg.attachments, index, cx));
    }

    // Parse content for thinking blocks (for assistant messages)
    // But skip this if we should render interleaved content instead
    if matches!(msg.role, MessageRole::Assistant) && !should_interleave {
        let segments = parse_content_segments(&msg.content);

        debug!(
            content_len = msg.content.len(),
            segments_count = segments.len(),
            has_thinking = segments.iter().any(|s| matches!(s, ContentSegment::Thinking(_))),
            content_preview = %msg.content.chars().take(100).collect::<String>(),
            "Parsing message for thinking blocks"
        );

        // If we have segments with thinking blocks, render them specially
        if segments
            .iter()
            .any(|s| matches!(s, ContentSegment::Thinking(_)))
        {
            let children: Vec<AnyElement> = segments
                .iter()
                .enumerate()
                .flat_map(|(seg_idx, segment)| match segment {
                    ContentSegment::Thinking(content) => {
                        vec![render_thinking_block(content, index, seg_idx, cx).into_any_element()]
                    }
                    ContentSegment::Text(text) => {
                        if msg.is_markdown && !msg.is_streaming {
                            // Parse this text segment for math
                            render_content_with_code_blocks(text, index * 100 + seg_idx, cx)
                        } else {
                            vec![div().child(text.clone()).into_any_element()]
                        }
                    }
                })
                .collect();

            let message_with_content = container.children(children);

            // Wrap with copy button for assistant messages
            return match msg.role {
                MessageRole::Assistant => div()
                    .child(message_with_content)
                    .child(
                        div().flex().justify_end().pt_2().child(
                            Button::new(ElementId::Name(format!("copy-msg-{}", index).into()))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(CustomIcon::Copy))
                                .tooltip("Copy message")
                                .on_click({
                                    let content = msg.content.clone();
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            content.clone(),
                                        ));
                                    }
                                }),
                        ),
                    )
                    .into_any_element(),
                MessageRole::User => message_with_content.into_any_element(),
            };
        }
    }

    // Render content based on whether it's markdown (no thinking blocks found)
    // Only render as markdown if NOT streaming (to avoid re-parsing on every chunk)

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
            on_toggle_tool,
            cx,
        )
    } else if msg.is_markdown && !msg.is_streaming {
        // Parse for math expressions
        let content_elements = render_content_with_code_blocks(&msg.content, index, cx);
        container.children(content_elements)
    } else {
        // Use plain text for streaming messages for better performance
        container.child(msg.content.clone())
    };

    // Wrap with copy button for assistant messages
    match msg.role {
        MessageRole::Assistant => div()
            .child(final_container)
            .child(
                div().flex().justify_end().pt_2().child(
                    Button::new(ElementId::Name(format!("copy-msg-{}", index).into()))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(CustomIcon::Copy))
                        .tooltip("Copy message")
                        .on_click({
                            let content = msg.content.clone();
                            move |_event, _window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(content.clone()));
                            }
                        }),
                ),
            )
            .into_any_element(),
        MessageRole::User => final_container.into_any_element(),
    }
}
