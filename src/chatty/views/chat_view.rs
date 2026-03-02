#![allow(clippy::collapsible_if)]

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::skeleton::Skeleton;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, trace, warn};

use super::chat_input::{ChatInput, ChatInputState};
use super::message_component::{DisplayMessage, MessageRole, render_message};
use super::message_types::{
    ApprovalBlock, ApprovalState, SystemTrace, ThinkingBlock, ThinkingState, ToolCallBlock,
    ToolCallState, TraceItem, UserMessage,
};
use super::parsed_cache::{CachedParseResult, ParsedContentCache};
use super::trace_components::SystemTraceView;
use crate::chatty::models::MessageFeedback;
use crate::settings::models::models_store::ModelsModel;
use std::time::SystemTime;

/// Main chat view component
#[derive(Clone)]
pub struct PendingApprovalInfo {
    pub id: String,
    pub command: String,
    pub is_sandboxed: bool,
    pub conversation_id: String,
}

pub struct ChatView {
    chat_input_state: Entity<ChatInputState>,
    messages: Vec<DisplayMessage>,
    conversation_id: Option<String>,
    scroll_handle: ScrollHandle,
    pending_approval: Option<PendingApprovalInfo>,
    /// Tracks which tool calls are collapsed: (message_idx, tool_idx) -> collapsed
    collapsed_tool_calls: HashMap<(usize, usize), bool>,
    /// Cache for parsed message content (markdown, math, code highlighting)
    parsed_cache: ParsedContentCache,
    /// Cache of the last streaming parse result, to reuse code block highlighting
    /// across streaming renders. Cleared on stream finalization or conversation switch.
    streaming_parse_cache: Option<CachedParseResult>,
    /// When true, every render re-asserts scroll_to_bottom so that async
    /// layout changes (image loading, SVG math, code blocks) never leave
    /// the view stuck above the true bottom. Disabled when user scrolls up.
    stick_to_bottom: bool,
}

/// Events emitted by ChatView for actions that require app-level handling
#[derive(Clone, Debug)]
pub enum ChatViewEvent {
    /// User toggled feedback on a message (msg_index in display messages,
    /// history_index for the parallel array in the Conversation model)
    FeedbackChanged {
        history_index: usize,
        feedback: Option<MessageFeedback>,
    },
    /// User clicked "Regenerate" on an assistant message
    RegenerateMessage { history_index: usize },
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type a message...")
                .clean_on_escape()
                .auto_grow(2, 15)
        });

        let chat_input_state = cx.new(|_cx| ChatInputState::new(input.clone()));
        let scroll_handle = ScrollHandle::new();

        // Subscribe to input events to handle Enter key
        let state_for_enter = chat_input_state.clone();
        cx.subscribe(&input, move |_input_state, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { secondary } = event {
                // Only send on plain Enter (not Shift+Enter)
                if !secondary {
                    tracing::debug!("Enter key pressed, calling send_message");
                    state_for_enter.update(cx, |state, cx| {
                        state.send_message(cx);
                    });
                }
            }
        })
        .detach();

        // Focus the input immediately after creation
        chat_input_state.update(cx, |state, cx| {
            state.input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });

        Self {
            chat_input_state,
            messages: Vec::new(),
            conversation_id: None,
            scroll_handle,
            pending_approval: None,
            collapsed_tool_calls: HashMap::new(),
            parsed_cache: ParsedContentCache::new(),
            streaming_parse_cache: None,
            stick_to_bottom: true,
        }
    }

    /// Get the chat input state entity (for wiring callbacks)
    pub fn chat_input_state(&self) -> &Entity<ChatInputState> {
        &self.chat_input_state
    }

    /// Set the conversation ID for this view
    pub fn set_conversation_id(&mut self, conversation_id: String, cx: &mut Context<Self>) {
        self.conversation_id = Some(conversation_id);
        cx.notify();
    }

    /// Get the current conversation ID
    pub fn conversation_id(&self) -> Option<&String> {
        self.conversation_id.as_ref()
    }

    /// Add a user message to the chat
    pub fn add_user_message(
        &mut self,
        text: String,
        attachments: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        debug!(message = %text, attachment_count = attachments.len(), "Adding user message");

        self.messages.push(DisplayMessage {
            role: MessageRole::User,
            content: text.clone(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: None,
            is_markdown: true,
            attachments,
            feedback: None,
            history_index: None,
        });

        debug!(total_messages = self.messages.len(), "User message added");
        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Start an assistant message (for streaming)
    pub fn start_assistant_message(&mut self, cx: &mut Context<Self>) {
        debug!("Starting assistant message");

        self.messages.push(DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming: true,
            system_trace_view: None,
            live_trace: Some(SystemTrace::new()),
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        });

        debug!(
            total_messages = self.messages.len(),
            "Assistant message started"
        );
        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Append text to the current streaming assistant message
    pub fn append_assistant_text(&mut self, text: &str, cx: &mut Context<Self>) {
        debug!(
            text_len = text.len(),
            total_messages = self.messages.len(),
            "append_assistant_text called"
        );
        if let Some(last) = self.messages.last_mut() {
            debug!(
                is_streaming = last.is_streaming,
                content_len = last.content.len(),
                "Last message details"
            );
            if last.is_streaming {
                last.content.push_str(text);
                debug!(new_content_len = last.content.len(), "Text appended");
                cx.notify();
                self.scroll_if_sticky();
            } else {
                warn!("Last message NOT streaming, text dropped");
            }
        } else {
            warn!("No messages in view, text dropped");
        }
    }

    /// Finalize the current streaming assistant message
    pub fn finalize_assistant_message(&mut self, cx: &mut Context<Self>) {
        if let Some(last) = self.messages.last_mut() {
            last.is_streaming = false;

            // Finalize live trace - push final state to view entity
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone, cx);
                        cx.notify();
                    });
                }
            }

            // Clear live trace (it's now frozen in the view entity)
            last.live_trace = None;

            // Clear the streaming parse cache — finalized content uses the
            // persistent ParsedContentCache instead.
            self.streaming_parse_cache = None;

            // Scroll to bottom after finalization. The cached render may produce
            // different-height content (e.g. code blocks, math) compared to the
            // streaming render, so the scroll position needs to be updated.
            self.activate_sticky_scroll();

            cx.notify();
        }
    }

    /// Set the history_index on the last assistant DisplayMessage.
    ///
    /// Called after `finalize_response` adds the assistant message to the
    /// conversation model so the parallel-array index is known. Without this,
    /// feedback clicks on freshly-streamed messages would be silently dropped
    /// because the callback guards emission behind `if let Some(h_idx)`.
    pub fn set_last_assistant_history_index(
        &mut self,
        history_index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(last) = self.messages.last_mut() {
            if matches!(last.role, MessageRole::Assistant) {
                last.history_index = Some(history_index);
                cx.notify();
            }
        }
    }

    /// Set attachments on the last assistant DisplayMessage.
    /// Called after finalization when tool calls generated files (e.g. plots)
    /// that should be displayed inline in the assistant's response.
    pub fn set_last_assistant_attachments(
        &mut self,
        attachments: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if let Some(last) = self.messages.last_mut() {
            if matches!(last.role, MessageRole::Assistant) {
                last.attachments = attachments;
                cx.notify();
            }
        }
    }

    /// Mark the current streaming message as cancelled by the user
    pub fn mark_message_cancelled(&mut self, cx: &mut Context<Self>) {
        if let Some(last) = self.messages.last_mut() {
            if last.is_streaming {
                // Append cancellation notice to the message
                if !last.content.is_empty() {
                    last.content.push_str("\n\n");
                }
                last.content.push_str("*[Response cancelled by user]*");
                last.is_streaming = false;

                // Clear streaming parse cache
                self.streaming_parse_cache = None;

                // Finalize trace if present
                if let Some(ref mut trace) = last.live_trace {
                    trace.clear_active_tool();
                }
                last.live_trace = None;

                cx.notify();
            }
        }
    }

    /// Extract the current trace before finalizing (for persistence)
    pub fn extract_current_trace(&mut self) -> Option<SystemTrace> {
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
                return Some(trace.clone());
            }
        }
        None
    }

    /// Handle tool call started event
    pub fn handle_tool_call_started(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        debug!(tool_id = %id, tool_name = %name, "UI: handle_tool_call_started called");

        // Capture current message content as "text_before" for interleaved rendering
        let text_before = self
            .messages
            .last()
            .map(|msg| msg.content.clone())
            .unwrap_or_default();

        debug!(
            tool_id = %id,
            tool_name = %name,
            text_before_len = text_before.len(),
            text_before_preview = %text_before.chars().take(50).collect::<String>(),
            "Captured text_before for tool call"
        );

        let display_name = friendly_tool_name(&name);
        let tool_call = ToolCallBlock {
            id: id.clone(),
            tool_name: name,
            display_name,
            input: String::new(),
            output: None,
            output_preview: None,
            state: ToolCallState::Running,
            duration: None,
            text_before,
        };

        // Update live trace and create/update system_trace_view entity
        if let Some(last) = self.messages.last_mut() {
            debug!(
                has_last_message = true,
                is_streaming = last.is_streaming,
                has_live_trace = last.live_trace.is_some(),
                "Checking live_trace availability"
            );
            if last.is_streaming {
                if let Some(ref mut trace) = last.live_trace {
                    debug!("Adding tool call to live_trace");
                    let index = trace.items.len();
                    trace.add_tool_call(tool_call);
                    trace.set_active_tool(index);

                    // Create or update the trace view entity for rendering
                    let trace_clone = trace.clone();
                    if last.system_trace_view.is_none() {
                        // Create new SystemTraceView entity
                        let trace_view = cx.new(|_cx| SystemTraceView::new(trace_clone));

                        // Subscribe to its events
                        let chat_view_entity = cx.entity();
                        cx.subscribe(
                            &trace_view,
                            move |_chat_view,
                                  _trace_view,
                                  event: &super::message_types::TraceEvent,
                                  cx| {
                                let event_clone = event.clone();
                                let chat_view = chat_view_entity.clone();
                                cx.defer(move |cx| {
                                    chat_view.update(cx, |chat_view, cx| {
                                        chat_view.handle_trace_event(&event_clone, cx);
                                    });
                                });
                            },
                        )
                        .detach();

                        last.system_trace_view = Some(trace_view);
                    } else if let Some(ref view_entity) = last.system_trace_view {
                        view_entity.update(cx, |view, cx| {
                            view.update_trace(trace_clone, cx);
                            cx.notify();
                        });
                    }
                }
            } else {
                debug!("live_trace not available for tool call");
            }
        } else {
            debug!("Last message is not streaming");
        }

        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Helper method to update a tool call by ID in the live trace.
    /// This works even after active_tool_index has been cleared.
    ///
    /// Uses a two-pass reverse scan to handle non-unique tool call IDs
    /// (e.g., multiple "shell_execute" calls that share the same ID when
    /// rig-core doesn't provide a unique call_id):
    ///
    /// 1. First pass (reverse): find the LAST entry with matching ID that
    ///    is still in Running state — targets the most recent pending call.
    /// 2. Fallback pass (reverse): find the LAST entry with matching ID
    ///    regardless of state — handles late-arriving updates.
    fn update_tool_call_by_id<F>(&mut self, tool_id: &str, updater: F) -> bool
    where
        F: FnOnce(&mut ToolCallBlock),
    {
        let last_message = match self.messages.last_mut() {
            Some(msg) => msg,
            None => {
                warn!("update_tool_call_by_id: No messages found");
                return false;
            }
        };

        let trace = match last_message.live_trace.as_mut() {
            Some(t) => t,
            None => {
                warn!("update_tool_call_by_id: No live_trace in message");
                return false;
            }
        };

        // Pass 1 (reverse): find the last entry with matching ID still in Running state.
        // This correctly targets the most recent pending tool call when IDs are
        // non-unique (e.g., multiple "shell_execute" calls).
        for item in trace.items.iter_mut().rev() {
            if let super::message_types::TraceItem::ToolCall(tc) = item {
                if tc.id == tool_id && matches!(tc.state, ToolCallState::Running) {
                    updater(tc);
                    return true;
                }
            }
        }

        // Pass 2 (fallback, reverse): no Running entry found — update the last
        // entry with matching ID regardless of state.
        for item in trace.items.iter_mut().rev() {
            if let super::message_types::TraceItem::ToolCall(tc) = item {
                if tc.id == tool_id {
                    updater(tc);
                    return true;
                }
            }
        }

        warn!(
            "update_tool_call_by_id: Tool call with id={} not found in trace items",
            tool_id
        );
        false
    }

    /// Handle tool call input event
    pub fn handle_tool_call_input(
        &mut self,
        id: String,
        arguments: String,
        cx: &mut Context<Self>,
    ) {
        // Update tool call input by ID
        self.update_tool_call_by_id(&id, |tc| {
            tc.input = arguments.clone();
        });

        // Update trace view - it will emit event if state changes
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref trace) = last.live_trace {
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone, cx);
                    });
                }
            }
        }
    }

    /// Handle tool call result event
    pub fn handle_tool_call_result(&mut self, id: String, result: String, cx: &mut Context<Self>) {
        debug!(tool_id = %id, result_length = result.len(), "UI: handle_tool_call_result called");

        // Check if result indicates a denial or error
        let is_denied = result.to_lowercase().contains("denied by user")
            || result.to_lowercase().contains("execution denied");

        // Update trace by ID
        self.update_tool_call_by_id(&id, |tc| {
            tc.output = Some(result.clone());
            tc.state = if is_denied {
                ToolCallState::Error("Denied by user".to_string())
            } else {
                ToolCallState::Success
            };
        });

        // Update trace view - it will emit ToolCallStateChanged event automatically
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone, cx); // This emits event!
                    });
                }
            }
        }

        // No need for cx.notify() - event handler calls it
        // No need for manual auto-expand - event handler does it
    }

    /// Handle tool call error event
    pub fn handle_tool_call_error(&mut self, id: String, error: String, cx: &mut Context<Self>) {
        // Update tool call state by ID
        self.update_tool_call_by_id(&id, |tc| {
            tc.state = ToolCallState::Error(error.clone());
        });

        // Update trace view - it will emit ToolCallStateChanged event automatically
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone, cx); // This emits event!
                    });
                }
            }
        }

        // No need for cx.notify() or manual auto-expand - event handler does it
    }

    /// Handle events from SystemTraceView
    fn handle_trace_event(
        &mut self,
        event: &super::message_types::TraceEvent,
        cx: &mut Context<Self>,
    ) {
        use super::message_types::TraceEvent;

        match event {
            TraceEvent::ToolCallStateChanged {
                tool_id,
                old_state,
                new_state,
            } => {
                warn!(
                    "Tool call {} changed: {:?} → {:?}",
                    tool_id, old_state, new_state
                );

                // Don't auto-expand - let user expand with Cmd+D (Details button)
                // This keeps the UI cleaner by not expanding every tool call automatically

                // Notify to trigger re-render
                cx.notify();
            }
            TraceEvent::ToolCallOutputReceived { tool_id, .. } => {
                debug!("Tool call {} received output", tool_id);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Auto-expand a tool call by its ID
    /// Handle approval requested event
    pub fn handle_approval_requested(
        &mut self,
        id: String,
        command: String,
        is_sandboxed: bool,
        cx: &mut Context<Self>,
    ) {
        debug!(approval_id = %id, command = %command, sandboxed = is_sandboxed, "UI: handle_approval_requested called");

        // Set pending approval for floating bar (only if we have a conversation ID)
        if let Some(conv_id) = &self.conversation_id {
            self.pending_approval = Some(PendingApprovalInfo {
                id: id.clone(),
                command: command.clone(),
                is_sandboxed,
                conversation_id: conv_id.clone(),
            });
        }

        // Create approval block with pending state
        let approval = ApprovalBlock {
            id,
            command,
            is_sandboxed,
            state: ApprovalState::Pending,
            created_at: SystemTime::now(),
        };

        // Update live trace and create/update system_trace_view entity
        if let Some(last) = self.messages.last_mut() {
            if last.is_streaming {
                if let Some(ref mut trace) = last.live_trace {
                    debug!("Adding approval to live_trace");
                    let index = trace.items.len();
                    trace.add_approval(approval);
                    trace.set_active_tool(index);

                    // Create or update the trace view entity for rendering
                    let trace_clone = trace.clone();
                    if last.system_trace_view.is_none() {
                        // Create new SystemTraceView entity
                        let trace_view = cx.new(|_cx| SystemTraceView::new(trace_clone));

                        // Subscribe to its events
                        let chat_view_entity = cx.entity();
                        cx.subscribe(
                            &trace_view,
                            move |_chat_view,
                                  _trace_view,
                                  event: &super::message_types::TraceEvent,
                                  cx| {
                                let event_clone = event.clone();
                                let chat_view = chat_view_entity.clone();
                                cx.defer(move |cx| {
                                    chat_view.update(cx, |chat_view, cx| {
                                        chat_view.handle_trace_event(&event_clone, cx);
                                    });
                                });
                            },
                        )
                        .detach();

                        last.system_trace_view = Some(trace_view);
                    } else if let Some(ref view_entity) = last.system_trace_view {
                        view_entity.update(cx, |view, cx| {
                            view.update_trace(trace_clone, cx);
                            cx.notify();
                        });
                    }
                }
            }
        }

        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Handle approval resolved event
    pub fn handle_approval_resolved(&mut self, id: &str, approved: bool, cx: &mut Context<Self>) {
        debug!(approval_id = %id, approved = approved, "UI: handle_approval_resolved called");

        // Clear pending approval (hide floating bar)
        if let Some(ref pending) = self.pending_approval {
            if pending.id == id {
                self.pending_approval = None;
            }
        }

        // Update approval state in live trace
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                let new_state = if approved {
                    ApprovalState::Approved
                } else {
                    ApprovalState::Denied
                };
                trace.update_approval_state(id, new_state);

                // Clear active tool after resolution
                trace.clear_active_tool();

                // Push updated trace to view entity
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone, cx);
                        cx.notify();
                    });
                }
            }
        }

        cx.notify();
    }

    /// Handle thinking block started event
    #[allow(dead_code)]
    pub fn handle_thinking_started(&mut self, cx: &mut Context<Self>) {
        debug!("Thinking block started");

        let thinking = ThinkingBlock {
            content: String::new(),
            summary: String::new(),
            duration: None,
            state: ThinkingState::Processing,
        };

        // Update live trace
        if let Some(last) = self.messages.last_mut() {
            debug!(
                has_last_message = true,
                is_streaming = last.is_streaming,
                has_live_trace = last.live_trace.is_some(),
                "Checking live_trace availability"
            );
            if last.is_streaming {
                if let Some(ref mut trace) = last.live_trace {
                    debug!("Adding tool call to live_trace");
                    let index = trace.items.len();
                    trace.add_thinking(thinking);
                    trace.set_active_tool(index);
                }
            } else {
                debug!("live_trace not available for tool call");
            }
        } else {
            debug!("Last message is not streaming");
        }

        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Helper method to update the active thinking block in the live trace
    #[allow(dead_code)]
    fn update_thinking_trace<F>(&mut self, updater: F) -> bool
    where
        F: FnOnce(&mut ThinkingBlock),
    {
        let last_message = match self.messages.last_mut() {
            Some(msg) => msg,
            None => return false,
        };

        if !last_message.is_streaming {
            return false;
        }

        let trace = match last_message.live_trace.as_mut() {
            Some(t) => t,
            None => return false,
        };

        let active_idx = match trace.active_tool_index {
            Some(idx) => idx,
            None => return false,
        };

        let item = match trace.items.get_mut(active_idx) {
            Some(i) => i,
            None => return false,
        };

        if let TraceItem::Thinking(tb) = item {
            updater(tb);
            return true;
        }

        false
    }

    /// Handle thinking block content delta event
    #[allow(dead_code)]
    pub fn handle_thinking_delta(&mut self, delta: &str, cx: &mut Context<Self>) {
        self.update_thinking_trace(|tb| {
            tb.content.push_str(delta);
        });

        cx.notify();
        self.scroll_if_sticky();
    }

    /// Handle thinking block ended event
    #[allow(dead_code)]
    pub fn handle_thinking_ended(&mut self, cx: &mut Context<Self>) {
        debug!("Thinking block ended");

        self.update_thinking_trace(|tb| {
            tb.state = ThinkingState::Completed;
            // Generate a summary from the first line or first N characters
            tb.summary = tb
                .content
                .lines()
                .next()
                .map(|line| {
                    if line.len() > 50 {
                        format!("{}...", &line[..50])
                    } else {
                        line.to_string()
                    }
                })
                .unwrap_or_else(|| "Analysis complete".to_string());
        });

        // Clear active tool after thinking completes
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
            }
        }

        cx.notify();
    }

    /// Remove the last assistant message from the display (used for regeneration)
    pub fn remove_last_assistant_message(&mut self, cx: &mut Context<Self>) {
        if self
            .messages
            .last()
            .is_some_and(|m| matches!(m.role, MessageRole::Assistant))
        {
            self.messages.pop();
            cx.notify();
        }
    }

    /// Clear all messages from the chat view
    pub fn clear_messages(&mut self, cx: &mut Context<Self>) {
        self.messages.clear();
        self.parsed_cache.clear();
        self.streaming_parse_cache = None;
        cx.notify();
    }

    /// Load message history from a conversation
    pub fn load_history(
        &mut self,
        history: &[rig::completion::Message],
        traces: &[Option<serde_json::Value>],
        attachment_paths: &[Vec<PathBuf>],
        message_feedback: &[Option<crate::chatty::models::MessageFeedback>],
        cx: &mut Context<Self>,
    ) {
        use rig::completion::Message;

        // Clear any pending approval from previous conversation
        self.pending_approval = None;

        // Clear collapsed tool calls state from previous conversation
        self.collapsed_tool_calls.clear();

        // Clear parsed content cache from previous conversation
        self.parsed_cache.clear();

        self.messages.clear();

        for (idx, msg) in history.iter().enumerate() {
            let feedback = message_feedback.get(idx).cloned().flatten();
            match msg {
                Message::User { content, .. } => {
                    let user_msg = UserMessage::from_rig_content(content);
                    let attachments = attachment_paths.get(idx).cloned().unwrap_or_default();
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
                        super::message_types::AssistantMessage::from_rig_content(content);

                    // Eagerly create trace view from persisted JSON so tool traces
                    // are visible when reopening a conversation.
                    let system_trace_view =
                        traces
                            .get(idx)
                            .and_then(|t| t.as_ref())
                            .and_then(|trace_json| {
                                match serde_json::from_value::<super::message_types::SystemTrace>(
                                    trace_json.clone(),
                                ) {
                                    Ok(trace) if trace.has_items() => Some(cx.new(|_cx| {
                                        super::trace_components::SystemTraceView::new(trace)
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

                    let attachments = attachment_paths.get(idx).cloned().unwrap_or_default();
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
            }
        }

        self.activate_sticky_scroll();
        cx.notify();
    }

    /// Activate sticky-scroll mode. While active, every render pass will
    /// re-assert scroll_to_bottom so that async content changes (image
    /// loading, SVG math rendering, code block expansion) never leave
    /// the view stuck above the true bottom.
    ///
    /// Sticky mode is automatically disabled when the user scrolls up.
    fn activate_sticky_scroll(&mut self) {
        self.stick_to_bottom = true;
        self.scroll_handle.scroll_to_bottom();
    }

    /// If sticky-scroll is active, re-assert scroll_to_bottom for this frame.
    /// Used for incremental streaming updates — respects the user's decision
    /// to scroll up by not re-enabling sticky mode.
    fn scroll_if_sticky(&mut self) {
        if self.stick_to_bottom {
            self.scroll_handle.scroll_to_bottom();
        }
    }

    /// Handle approval decision from floating bar
    fn handle_floating_approval(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(ref pending) = self.pending_approval {
            let id = pending.id.clone();

            // Try execution approval store first (bash commands)
            let mut resolved = false;
            if let Some(store) = cx.try_global::<crate::chatty::models::execution_approval_store::ExecutionApprovalStore>() {
                use crate::chatty::models::execution_approval_store::ApprovalDecision;
                resolved = store.resolve(&id, if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                });
            }

            // If not found in execution store, try write approval store (filesystem writes)
            if !resolved {
                if let Some(store) = cx.try_global::<crate::chatty::models::WriteApprovalStore>() {
                    use crate::chatty::models::write_approval_store::WriteApprovalDecision;
                    store.resolve(
                        &id,
                        if approved {
                            WriteApprovalDecision::Approved
                        } else {
                            WriteApprovalDecision::Denied
                        },
                    );
                }
            }

            // Immediately clear pending approval to hide the bar
            self.pending_approval = None;

            // Also update the trace
            self.handle_approval_resolved(&id, approved, cx);
        }
    }

    /// Expand trace and scroll to approval for "View Details" button
    fn expand_trace_to_approval(&mut self, cx: &mut Context<Self>) {
        trace!("expand_trace_to_approval called");

        if let Some(last) = self.messages.last_mut() {
            if let Some(ref view_entity) = last.system_trace_view {
                view_entity.update(cx, |view, cx| {
                    view.set_collapsed(false);
                    cx.notify();
                });

                self.activate_sticky_scroll();
                trace!("Trace expanded and scrolled");
            } else {
                trace!("No system_trace_view found - trace not created yet");
            }
        }
    }

    /// Check if we're awaiting a response (streaming message with no content yet
    /// and no tool calls in progress)
    fn is_awaiting_response(&self) -> bool {
        self.messages.last().is_some_and(|msg| {
            msg.is_streaming
                && msg.content.is_empty()
                && !msg
                    .live_trace
                    .as_ref()
                    .is_some_and(|trace| trace.has_items())
        })
    }

    /// Render loading skeleton indicator
    fn render_loading_skeleton(&self) -> impl IntoElement {
        div()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .child(Skeleton::new().w(px(280.)).h(px(16.)).rounded(px(4.)))
            .child(Skeleton::new().w(px(220.)).h(px(16.)).rounded(px(4.)))
            .child(Skeleton::new().w(px(180.)).h(px(16.)).rounded(px(4.)))
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Sticky-scroll: re-assert scroll_to_bottom on every render so that
        // async layout changes (image loading, SVG math, code blocks) always
        // converge to the true bottom. Detect user scroll-away to disable.
        if self.stick_to_bottom {
            // offset() and max_offset() both reflect the last prepaint, so
            // they are consistent. Content growth doesn't cause a false
            // positive because offset was set to -max_offset in that same
            // prepaint; only a user scroll event can move offset away.
            let offset = self.scroll_handle.offset();
            let max_offset = self.scroll_handle.max_offset();
            let distance_from_bottom = max_offset.height + offset.y;

            if distance_from_bottom > px(10.0) && max_offset.height > px(0.0) {
                // User scrolled away from bottom — disable sticky mode
                self.stick_to_bottom = false;
                trace!(
                    distance = %distance_from_bottom,
                    "Sticky scroll disabled: user scrolled up"
                );
            } else {
                // Still at bottom. Re-assert so THIS frame's (possibly larger)
                // content_size will be used by clamp_scroll_position.
                self.scroll_handle.scroll_to_bottom();
            }
        }

        // Clear the input if a message was sent
        self.chat_input_state.update(cx, |state, cx| {
            state.clear_if_needed(window, cx);
        });

        // Auto-create first conversation if needed (one-time check)
        use crate::chatty::models::ConversationsStore;
        if self.conversation_id.is_none() {
            if let Some(convs_model) = cx.try_global::<ConversationsStore>() {
                if convs_model.count() == 0
                    && !cx
                        .try_global::<ModelsModel>()
                        .map(|m| m.models().is_empty())
                        .unwrap_or(true)
                {
                    info!("No conversations and models available, triggering creation");
                    // We need to trigger conversation creation on the parent ChattyApp
                    // This will be handled by sending a signal
                }
            }
        }

        // Refresh available models from global store (in case they changed)
        if let Some(models_model) = cx.try_global::<ModelsModel>() {
            let models_list: Vec<(String, String)> = models_model
                .models()
                .iter()
                .map(|m| (m.id.clone(), m.name.clone()))
                .collect();

            if !models_list.is_empty() {
                self.chat_input_state.update(cx, |state, _cx| {
                    // Only update if the list is different or empty
                    if state.available_models().is_empty()
                        || state.available_models() != models_list.as_slice()
                    {
                        let default_model_id = models_list.first().map(|(id, _)| id.clone());
                        state.set_available_models(models_list, default_model_id);
                    }
                });
            }
        }

        let has_pending_approval = self.pending_approval.is_some();
        let view_entity_for_keys = cx.entity();
        let pending_conv_id = self
            .pending_approval
            .as_ref()
            .map(|p| p.conversation_id.clone());
        let current_conv_id = self.conversation_id.clone();

        div()
            .flex_1()
            .h_full()
            .w_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .overflow_hidden()
            // Add top padding on macOS for floating toggle button
            .when(cfg!(target_os = "macos"), |this| this.pt(px(24.)))
            // Handle keyboard shortcuts for approval bar
            .when(has_pending_approval, |this| {
                this.on_key_down(move |event: &KeyDownEvent, _window, cx| {
                    let modifiers = event.keystroke.modifiers;
                    let key = &event.keystroke.key;

                    warn!(
                        "ChatView key down: key={}, platform={}",
                        key, modifiers.platform
                    );

                    // Validate that the pending approval belongs to the current conversation
                    if pending_conv_id.as_ref() != current_conv_id.as_ref() {
                        warn!(
                            "Ignoring keyboard shortcut: approval belongs to different conversation (pending: {:?}, current: {:?})",
                            pending_conv_id, current_conv_id
                        );
                        return;
                    }

                    // Use platform modifier (Cmd on macOS, Ctrl elsewhere)
                    if modifiers.platform {
                        warn!("Platform modifier pressed with key: {}", key);
                        match key.as_str() {
                            "y" => {
                                warn!("Approve shortcut triggered in ChatView");
                                view_entity_for_keys.update(cx, |view, cx| {
                                    view.handle_floating_approval(true, cx);
                                });
                                cx.stop_propagation();
                            }
                            "n" if modifiers.shift => {
                                warn!("Deny shortcut triggered in ChatView");
                                view_entity_for_keys.update(cx, |view, cx| {
                                    view.handle_floating_approval(false, cx);
                                });
                                cx.stop_propagation();
                            }
                            "d" => {
                                warn!("Details shortcut triggered in ChatView");
                                view_entity_for_keys.update(cx, |view, cx| {
                                    view.expand_trace_to_approval(cx);
                                });
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    }
                })
            })
            .child(
                // Message list - scrollable area
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child({
                        let is_awaiting = self.is_awaiting_response();
                        div()
                            .id("chat-messages")
                            .track_scroll(&self.scroll_handle)
                            .overflow_scroll()
                            .size_full()
                            .child(
                                div()
                                    .p_4()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    .children({
                                        let collapsed_tool_calls =
                                            self.collapsed_tool_calls.clone();
                                        let chat_view_entity = cx.entity();

                                        // Temporarily move caches out to avoid split borrow
                                        // (self.messages is borrowed immutably below)
                                        let mut parsed_cache =
                                            std::mem::take(&mut self.parsed_cache);
                                        let mut streaming_cache =
                                            self.streaming_parse_cache.take();

                                        // Compute visible messages and find the last visible assistant index
                                        let visible_messages: Vec<(usize, &DisplayMessage)> =
                                            self.messages
                                                .iter()
                                                .enumerate()
                                                .filter(|(_, msg)| {
                                                    // Skip empty streaming messages that have no
                                                    // tool calls yet (we show skeleton instead).
                                                    // Once tool calls arrive the message must be
                                                    // visible so the trace/approval bar is shown.
                                                    !(msg.is_streaming
                                                        && msg.content.is_empty()
                                                        && !msg
                                                            .live_trace
                                                            .as_ref()
                                                            .is_some_and(|trace| trace.has_items()))
                                                })
                                                .collect();

                                        let last_visible_assistant_idx = visible_messages
                                            .iter()
                                            .rev()
                                            .find(|(_, msg)| {
                                                matches!(msg.role, MessageRole::Assistant)
                                                    && !msg.is_streaming
                                                    && msg.live_trace.is_none()
                                            })
                                            .map(|(idx, _)| *idx);

                                        let rendered: Vec<_> = visible_messages
                                            .into_iter()
                                            .map(|(index, msg)| {
                                                let entity_clone = chat_view_entity.clone();
                                                let entity_for_feedback = chat_view_entity.clone();
                                                let entity_for_regenerate = chat_view_entity.clone();
                                                let history_index = msg.history_index;
                                                let is_last_message = last_visible_assistant_idx == Some(index);
                                                // Only pass the streaming cache for the
                                                // active streaming message; non-streaming
                                                // messages use the persistent parsed_cache.
                                                let mut no_cache: Option<CachedParseResult> = None;
                                                let sc = if msg.is_streaming {
                                                    &mut streaming_cache
                                                } else {
                                                    &mut no_cache
                                                };
                                                render_message(
                                                    msg,
                                                    index,
                                                    is_last_message,
                                                    &collapsed_tool_calls,
                                                    &mut parsed_cache,
                                                    sc,
                                                    move |msg_idx, tool_idx, cx| {
                                                        entity_clone.update(cx, |chat_view, cx| {
                                                            let key = (msg_idx, tool_idx);
                                                            let current = chat_view
                                                                .collapsed_tool_calls
                                                                .get(&key)
                                                                .copied()
                                                                .unwrap_or(true);

                                                            chat_view
                                                                .collapsed_tool_calls
                                                                .insert(key, !current);
                                                            cx.notify();
                                                        });
                                                    },
                                                    move |msg_idx, feedback, cx| {
                                                        entity_for_feedback.update(cx, |chat_view, cx| {
                                                            // Update display state
                                                            if let Some(display_msg) = chat_view.messages.get_mut(msg_idx) {
                                                                display_msg.feedback = feedback.clone();
                                                            }
                                                            // Emit event for persistence
                                                            if let Some(h_idx) = history_index {
                                                                cx.emit(ChatViewEvent::FeedbackChanged {
                                                                    history_index: h_idx,
                                                                    feedback,
                                                                });
                                                            }
                                                            cx.notify();
                                                        });
                                                    },
                                                    move |_msg_idx, cx| {
                                                        entity_for_regenerate.update(cx, |_chat_view, cx| {
                                                            if let Some(h_idx) = history_index {
                                                                cx.emit(ChatViewEvent::RegenerateMessage {
                                                                    history_index: h_idx,
                                                                });
                                                            }
                                                        });
                                                    },
                                                    cx,
                                                )
                                            })
                                            .collect();

                                        // Move caches back
                                        self.parsed_cache = parsed_cache;
                                        self.streaming_parse_cache = streaming_cache;

                                        rendered
                                    })
                                    .when(is_awaiting, |this| {
                                        this.child(self.render_loading_skeleton())
                                    }),
                            )
                    })
                    .vertical_scrollbar(&self.scroll_handle),
            )
            // Floating approval bar (if pending and belongs to current conversation)
            .when_some(
                self.pending_approval.as_ref().filter(|approval| {
                    // Only show approval if it belongs to current conversation
                    self.conversation_id.as_ref() == Some(&approval.conversation_id)
                }).cloned(),
                |this, pending| {
                    let view_entity = cx.entity();
                    this.child(
                    div().child(
                        super::approval_prompt_bar::ApprovalPromptBar::new(
                            pending.command,
                            pending.is_sandboxed,
                        )
                        .on_approve_deny({
                            let entity = view_entity.clone();
                            move |approved, cx| {
                                entity.update(cx, |view, cx| {
                                    view.handle_floating_approval(approved, cx);
                                });
                            }
                        })
                        .on_expand({
                            let entity = view_entity.clone();
                            move |cx| {
                                entity.update(cx, |view, cx| {
                                    view.expand_trace_to_approval(cx);
                                });
                            }
                        }),
                    ),
                )
            })
            .child(
                // Chat input - fixed at bottom
                div()
                    .flex_shrink_0()
                    .p_4()
                    .child(ChatInput::new(self.chat_input_state.clone())),
            )
    }
}

/// Map raw tool names to user-friendly display names
fn friendly_tool_name(name: &str) -> String {
    match name {
        "read_file" => "Reading file".to_string(),
        "read_binary" => "Reading binary file".to_string(),
        "list_directory" => "Listing directory".to_string(),
        "glob_search" => "Searching files".to_string(),
        "write_file" => "Writing file".to_string(),
        "create_directory" => "Creating directory".to_string(),
        "delete_file" => "Deleting file".to_string(),
        "move_file" => "Moving file".to_string(),
        "apply_diff" => "Applying diff".to_string(),
        "shell_execute" => "Running command".to_string(),
        other => other.to_string(),
    }
}
