use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::text::TextView;

use super::message_types::{AssistantMessage, SystemTrace};
use super::trace_components::SystemTraceView;

/// Message role indicator
#[derive(Clone, Debug)]
pub enum MessageRole {
    User,
    Assistant,
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

    // Render content based on whether it's markdown
    // Only render as markdown if NOT streaming (to avoid re-parsing on every chunk)
    if msg.is_markdown && !msg.is_streaming && matches!(msg.role, MessageRole::Assistant) {
        container.child(MarkdownContent {
            content: msg.content.clone(),
            message_index: index,
        })
    } else {
        // Use plain text for streaming messages for better performance
        container.child(msg.content.clone())
    }
}
