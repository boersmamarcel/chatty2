use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::text::TextView;
use std::path::PathBuf;
use tracing::{debug, info};

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
fn render_math_aware_content(content: &str, base_index: usize, _cx: &App) -> Vec<AnyElement> {
    info!(content_len = content.len(), base_index, "render_math_aware_content called");

    // Parse for math segments
    let math_segments = parse_math_segments(content);

    // Log what we found
    for (idx, seg) in math_segments.iter().enumerate() {
        match seg {
            MathSegment::Text(t) => info!(idx, text_len = t.len(), "Text segment"),
            MathSegment::InlineMath(m) => info!(idx, math = %m, "Inline math segment"),
            MathSegment::BlockMath(m) => info!(idx, math = %m, "Block math segment"),
        }
    }

    let mut elements = Vec::new();
    let mut inline_row: Vec<AnyElement> = Vec::new();
    let mut seg_idx = 0;

    for segment in math_segments.iter() {
        let element_index = base_index * 1000 + seg_idx;
        seg_idx += 1;

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
                inline_row.push(
                    MathComponent::new(math_content.clone(), true, element_id).into_any_element(),
                );
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
                            .into_any_element()
                    );
                }
                
                // Render block math as its own element
                let element_id = ElementId::Name(format!("math-block-{}", element_index).into());
                elements.push(
                    MathComponent::new(math_content.clone(), false, element_id).into_any_element(),
                );
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
                .into_any_element()
        );
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

/// Render a message in the chat view
pub fn render_message(msg: &DisplayMessage, index: usize, cx: &App) -> impl IntoElement {
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

    // Add system trace if present (for tool calls, thinking, etc.)
    if let Some(ref trace_view) = msg.system_trace_view {
        container = container.child(trace_view.clone());
    }

    // Render attachments (images/PDFs) if present
    if !msg.attachments.is_empty() {
        container = container.child(render_attachments(&msg.attachments, index, cx));
    }

    // Parse content for thinking blocks (for assistant messages)
    if matches!(msg.role, MessageRole::Assistant) {
        let segments = parse_content_segments(&msg.content);

        info!(
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
                            render_math_aware_content(text, index * 100 + seg_idx, cx)
                        } else {
                            vec![div().child(text.clone()).into_any_element()]
                        }
                    }
                })
                .collect();

            return container.children(children);
        }
    }

    // Render content based on whether it's markdown (no thinking blocks found)
    // Only render as markdown if NOT streaming (to avoid re-parsing on every chunk)
    info!(
        is_markdown = msg.is_markdown,
        is_streaming = msg.is_streaming,
        role = ?msg.role,
        "Checking render conditions"
    );
    if msg.is_markdown && !msg.is_streaming && matches!(msg.role, MessageRole::Assistant) {
        info!("Conditions met - calling render_math_aware_content");
        // Parse for math expressions
        let math_aware_elements = render_math_aware_content(&msg.content, index, cx);
        container.children(math_aware_elements)
    } else {
        info!("Conditions NOT met - using plain text");
        // Use plain text for streaming messages for better performance
        container.child(msg.content.clone())
    }
}



