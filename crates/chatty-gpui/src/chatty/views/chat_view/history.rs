//! Conversation-history loading for `ChatView`.
//!
//! # What lives here
//!
//! Just `load_history` — replaces all in-view state with messages
//! deserialized from a `chatty_core::models::MessageEntry` slice. Run
//! when the user switches to a different conversation or reopens the
//! app.
//!
//! # What does NOT live here
//!
//! - Adding a single message during a live stream — `add_user_message`,
//!   `start_assistant_message`, etc. in `mod.rs`.
//! - Persisting messages — that's `Conversation` / `ConversationRepository`
//!   in `chatty-core`; this view only consumes the data.

use gpui::*;

use super::super::message_component::{DisplayMessage, MessageRole};
use super::super::message_types::{SystemTrace, UserMessage};
use super::ChatView;

impl ChatView {
    /// Load message history from a conversation
    pub fn load_history(
        &mut self,
        entries: &[chatty_core::models::MessageEntry],
        cx: &mut Context<Self>,
    ) {
        use rig_core::completion::Message;

        // Clear any pending approval from previous conversation
        self.pending_approval = None;
        self.clear_agent_task_snapshot(cx);

        // Clear collapsed tool calls state from previous conversation
        self.collapsed_tool_calls.clear();
        self.diff_expanded.clear();

        // Clear parsed content cache from previous conversation
        self.parsed_cache.clear();

        // Reset sub-agent tracking (sub-agent progress is UI-only, not in history)
        self.sub_agent_progress_msg_idx = None;

        self.messages.clear();

        for (idx, entry) in entries.iter().enumerate() {
            let feedback = entry.feedback.clone();
            match &entry.message {
                Message::User { content, .. } => {
                    let user_msg = UserMessage::from_rig_content(content);
                    let attachments = entry.attachment_paths.clone();
                    if !user_msg.text.is_empty() || !attachments.is_empty() {
                        self.messages.push(DisplayMessage {
                            role: MessageRole::User,
                            content: user_msg.text,
                            is_streaming: false,
                            system_trace_view: None,
                            live_trace: None,
                            is_markdown: true,
                            attachments,
                            feedback: None,
                            history_index: Some(idx),
                        });
                    }
                }
                Message::Assistant { content, .. } => {
                    let assistant_msg =
                        super::super::message_types::AssistantMessage::from_rig_content(content);

                    // Eagerly create trace view from persisted JSON so tool traces
                    // are visible when reopening a conversation.
                    let system_trace_view = entry.system_trace.as_ref().and_then(|trace_json| {
                        match serde_json::from_value::<SystemTrace>(trace_json.clone()) {
                            Ok(trace) if trace.has_items() => Some(cx.new(|_cx| {
                                super::super::trace_components::SystemTraceView::new(trace)
                            })),
                            Ok(_) => None, // trace exists but has no items
                            Err(e) => {
                                tracing::warn!(
                                    idx,
                                    error = ?e,
                                    json_preview = %format!("{:.200}", trace_json),
                                    "Failed to deserialize SystemTrace in load_history"
                                );
                                None
                            }
                        }
                    });

                    let attachments = entry.attachment_paths.clone();
                    if !assistant_msg.text.is_empty() || !attachments.is_empty() {
                        self.messages.push(DisplayMessage {
                            role: MessageRole::Assistant,
                            content: assistant_msg.text.clone(),
                            is_streaming: false,
                            system_trace_view,
                            live_trace: None,
                            is_markdown: true,
                            attachments,
                            feedback,
                            history_index: Some(idx),
                        });
                    }
                }
                Message::System { .. } => {
                    // System messages are not rendered in the chat view
                }
            }
        }

        self.activate_sticky_scroll();
        cx.notify();
    }
}
