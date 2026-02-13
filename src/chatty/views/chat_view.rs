#![allow(clippy::collapsible_if)]

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::skeleton::Skeleton;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

use super::chat_input::{ChatInput, ChatInputState};
use super::message_component::{DisplayMessage, MessageRole, render_message};
use super::message_types::{
    ApprovalBlock, ApprovalState, SystemTrace, ThinkingBlock, ThinkingState, ToolCallBlock,
    ToolCallState, TraceItem, UserMessage,
};
use super::trace_components::SystemTraceView;
use crate::settings::models::models_store::ModelsModel;
use std::time::SystemTime;

/// Main chat view component
#[derive(Clone)]
pub struct PendingApprovalInfo {
    pub id: String,
    pub command: String,
    pub is_sandboxed: bool,
}

pub struct ChatView {
    chat_input_state: Entity<ChatInputState>,
    messages: Vec<DisplayMessage>,
    conversation_id: Option<String>,
    scroll_handle: ScrollHandle,
    active_tool_calls: HashMap<String, ToolCallBlock>,
    pending_approval: Option<PendingApprovalInfo>,
    /// Tracks which tool calls are collapsed: (message_idx, tool_idx) -> collapsed
    collapsed_tool_calls: HashMap<(usize, usize), bool>,
}

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
            active_tool_calls: HashMap::new(),
            pending_approval: None,
            collapsed_tool_calls: HashMap::new(),
        }
    }

    /// Get the chat input state entity (for wiring callbacks)
    pub fn chat_input_state(&self) -> &Entity<ChatInputState> {
        &self.chat_input_state
    }

    /// Set the conversation ID for this view
    pub fn set_conversation_id(&mut self, conversation_id: String) {
        self.conversation_id = Some(conversation_id);
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
        });

        debug!(total_messages = self.messages.len(), "User message added");
        cx.notify();
        self.scroll_to_bottom();
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
        });
        self.active_tool_calls.clear();

        debug!(
            total_messages = self.messages.len(),
            "Assistant message started"
        );
        cx.notify();
        self.scroll_to_bottom();
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
                self.scroll_to_bottom();
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
                        view.update_trace(trace_clone);
                        cx.notify();
                    });
                }
            }

            // Clear live trace (it's now frozen in the view entity)
            last.live_trace = None;

            cx.notify();
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

        self.active_tool_calls.insert(id, tool_call.clone());

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
                        last.system_trace_view =
                            Some(cx.new(|_cx| SystemTraceView::new(trace_clone)));
                    } else if let Some(ref view_entity) = last.system_trace_view {
                        view_entity.update(cx, |view, cx| {
                            view.update_trace(trace_clone);
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
        self.scroll_to_bottom();
    }

    /// Helper method to update a tool call by ID in the live trace
    /// This works even after active_tool_index has been cleared
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

        // Find the tool call by ID in the items
        for item in trace.items.iter_mut() {
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

    /// Helper method to update the active tool call in the live trace
    /// Reduces nesting from 6 levels to 2
    fn update_tool_call_trace<F>(&mut self, updater: F) -> bool
    where
        F: FnOnce(&mut ToolCallBlock),
    {
        let last_message = match self.messages.last_mut() {
            Some(msg) => msg,
            None => {
                warn!("update_tool_call_trace: No messages found");
                return false;
            }
        };

        // Allow updates even when not streaming - tool results can arrive after message finalization
        let is_streaming = last_message.is_streaming;
        if !is_streaming {
            warn!("update_tool_call_trace: Message is not streaming, will try to update anyway");
        }

        let trace = match last_message.live_trace.as_mut() {
            Some(t) => t,
            None => {
                warn!("update_tool_call_trace: No live_trace in message");
                return false;
            }
        };

        let active_idx = match trace.active_tool_index {
            Some(idx) => idx,
            None => {
                warn!("update_tool_call_trace: No active_tool_index, cannot update");
                return false;
            }
        };

        let item = match trace.items.get_mut(active_idx) {
            Some(i) => i,
            None => return false,
        };

        if let super::message_types::TraceItem::ToolCall(tc) = item {
            updater(tc);
            return true;
        }

        false
    }

    /// Handle tool call input event
    pub fn handle_tool_call_input(
        &mut self,
        id: String,
        arguments: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(tool_call) = self.active_tool_calls.get_mut(&id) {
            tool_call.input = arguments.clone();
        }

        self.update_tool_call_trace(|tc| {
            tc.input = arguments;
        });

        // Push updated trace to the view entity
        if let Some(last) = self.messages.last_mut() {
            if last.is_streaming {
                if let Some(ref trace) = last.live_trace {
                    let trace_clone = trace.clone();
                    if let Some(ref view_entity) = last.system_trace_view {
                        view_entity.update(cx, |view, cx| {
                            view.update_trace(trace_clone);
                            cx.notify();
                        });
                    }
                }
            }
        }

        cx.notify();
    }

    /// Handle tool call result event
    pub fn handle_tool_call_result(&mut self, id: String, result: String, cx: &mut Context<Self>) {
        debug!(tool_id = %id, result_length = result.len(), "UI: handle_tool_call_result called");

        if let Some(tool_call) = self.active_tool_calls.get_mut(&id) {
            debug!("Found active tool call, updating result");
            tool_call.output = Some(result.clone());
            tool_call.output_preview = Some(result.clone());
            tool_call.state = ToolCallState::Success;
        }

        // Use ID-based update instead of active_tool_index (which may have been cleared)
        let updated = self.update_tool_call_by_id(&id, |tc| {
            warn!(
                "UPDATING tool call in trace: old_state={:?}, setting to Success",
                tc.state
            );
            tc.output = Some(result.clone());
            tc.state = ToolCallState::Success;
        });

        warn!(
            "update_tool_call_by_id returned: {} for tool_id={}",
            updated, id
        );

        // Auto-expand the tool call when it completes successfully
        if updated {
            // Find the message index and tool index to auto-expand
            if let Some(msg_idx) = self.messages.len().checked_sub(1) {
                if let Some(last) = self.messages.last() {
                    if let Some(ref trace) = last.live_trace {
                        // Find the tool index by ID
                        for (tool_idx, item) in trace.items.iter().enumerate() {
                            if let super::message_types::TraceItem::ToolCall(tc) = item {
                                if tc.id == id {
                                    // Auto-expand this tool call
                                    let key = (msg_idx, tool_idx);
                                    self.collapsed_tool_calls.insert(key, false);
                                    warn!("Auto-expanded tool call at {:?}", key);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Clear active tool after successful completion and push to view entity
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone);
                        cx.notify();
                    });
                }
            }
        }

        debug!("Tool call result handled, notifying ChatView to re-render");
        cx.notify();
    }

    /// Handle tool call error event
    pub fn handle_tool_call_error(&mut self, id: String, error: String, cx: &mut Context<Self>) {
        if let Some(tool_call) = self.active_tool_calls.get_mut(&id) {
            tool_call.state = ToolCallState::Error(error.clone());
        }

        self.update_tool_call_trace(|tc| {
            tc.state = ToolCallState::Error(error);
        });

        // Clear active tool after error and push to view entity
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone);
                        cx.notify();
                    });
                }
            }
        }

        cx.notify();
    }

    /// Handle approval requested event
    pub fn handle_approval_requested(
        &mut self,
        id: String,
        command: String,
        is_sandboxed: bool,
        cx: &mut Context<Self>,
    ) {
        debug!(approval_id = %id, command = %command, sandboxed = is_sandboxed, "UI: handle_approval_requested called");

        // Set pending approval for floating bar
        self.pending_approval = Some(PendingApprovalInfo {
            id: id.clone(),
            command: command.clone(),
            is_sandboxed,
        });

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
                        last.system_trace_view =
                            Some(cx.new(|_cx| SystemTraceView::new(trace_clone)));
                    } else if let Some(ref view_entity) = last.system_trace_view {
                        view_entity.update(cx, |view, cx| {
                            view.update_trace(trace_clone);
                            cx.notify();
                        });
                    }
                }
            }
        }

        cx.notify();
        self.scroll_to_bottom();
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
                        view.update_trace(trace_clone);
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
        self.scroll_to_bottom();
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
        self.scroll_to_bottom();
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

    /// Clear all messages from the chat view
    pub fn clear_messages(&mut self, cx: &mut Context<Self>) {
        self.messages.clear();
        cx.notify();
    }

    /// Load message history from a conversation
    pub fn load_history(
        &mut self,
        history: &[rig::completion::Message],
        traces: &[Option<serde_json::Value>],
        attachment_paths: &[Vec<PathBuf>],
        cx: &mut Context<Self>,
    ) {
        use rig::completion::Message;

        self.messages.clear();

        for (idx, msg) in history.iter().enumerate() {
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
                        });
                    }
                }
                Message::Assistant { content, .. } => {
                    let mut assistant_msg =
                        super::message_types::AssistantMessage::from_rig_content(content);

                    // Restore system trace if available
                    if let Some(Some(trace_json)) = traces.get(idx) {
                        if let Some(msg_with_trace) =
                            super::message_types::AssistantMessage::with_trace_json(
                                assistant_msg.text.clone(),
                                trace_json,
                            )
                        {
                            assistant_msg = msg_with_trace;
                        }
                    }

                    if !assistant_msg.text.is_empty() {
                        self.messages
                            .push(DisplayMessage::from_assistant_message(&assistant_msg, cx));
                    }
                }
            }
        }

        cx.notify();
    }

    /// Scroll to the bottom of the message list
    fn scroll_to_bottom(&mut self) {
        self.scroll_handle.set_offset(point(px(0.0), px(-f32::MAX)));
    }

    /// Handle approval decision from floating bar
    fn handle_floating_approval(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(ref pending) = self.pending_approval {
            let id = pending.id.clone();

            // Resolve in approval store
            if let Some(store) = cx.try_global::<crate::chatty::models::execution_approval_store::ExecutionApprovalStore>() {
                use crate::chatty::models::execution_approval_store::ApprovalDecision;
                store.resolve(&id, if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                });
            }

            // Immediately clear pending approval to hide the bar
            self.pending_approval = None;

            // Also update the trace
            self.handle_approval_resolved(&id, approved, cx);
        }
    }

    /// Expand trace and scroll to approval for "View Details" button
    fn expand_trace_to_approval(&mut self, cx: &mut Context<Self>) {
        if let Some(last) = self.messages.last_mut() {
            if let Some(ref view_entity) = last.system_trace_view {
                view_entity.update(cx, |view, cx| {
                    view.set_collapsed(false); // Expand trace
                    cx.notify();
                });
            }
        }
        self.scroll_to_bottom();
    }

    /// Check if we're awaiting a response (streaming message with no content yet)
    fn is_awaiting_response(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|msg| msg.is_streaming && msg.content.is_empty())
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

                                        self.messages
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, msg)| {
                                                // Skip empty streaming messages (we show skeleton instead)
                                                !(msg.is_streaming && msg.content.is_empty())
                                            })
                                            .map(|(index, msg)| {
                                                let entity_clone = chat_view_entity.clone();
                                                render_message(
                                                    msg,
                                                    index,
                                                    &collapsed_tool_calls,
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
                                                    cx,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    })
                                    .when(is_awaiting, |this| {
                                        this.child(self.render_loading_skeleton())
                                    }),
                            )
                    })
                    .vertical_scrollbar(&self.scroll_handle),
            )
            // Floating approval bar (if pending)
            .when_some(self.pending_approval.clone(), |this, pending| {
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
        "bash" => "Running command".to_string(),
        other => other.to_string(),
    }
}
