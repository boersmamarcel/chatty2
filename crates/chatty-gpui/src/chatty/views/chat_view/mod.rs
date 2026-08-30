//! Main chat view — the central pane that lists messages, attachments,
//! tool-call trace components, and the chat input.
//!
//! # What lives here
//!
//! - `ChatView` entity + render path (message list, scroll handling,
//!   skeleton placeholder, attachment thumbnails).
//! - `ChatViewEvent` — events emitted up to `ChattyApp` (scroll, copy,
//!   regenerate, edit, etc.).
//! - Helpers for streaming text into the active assistant message
//!   (`append_assistant_text`, `set_assistant_tool_call`, …).
//!
//! # What does NOT live here
//!
//! - Message data — `chatty_core::models::conversation::Message`.
//! - The chat input field — `chat_input.rs`.
//! - Individual message rendering — `message_component.rs`.
//! - Stream lifecycle — `chatty::models::stream_manager`; this view only
//!   receives already-decoded text/tool-call chunks via `ChattyApp` event
//!   handlers.
//! - Code blocks, diff views, math, mermaid — dedicated `*_component.rs`
//!   files under this directory.
//!
//! See `docs/rendering-system.md` and `docs/stream-manager.md`.
//!
//! # Submodules
//!
//! For agent-friendly navigation, `ChatView`'s `impl` blocks are split
//! across child modules that group methods by responsibility. Public
//! API is unchanged — every `pub fn` is still accessible as
//! `ChatView::foo(...)` from outside this module.
//!
//! - [`handlers`] — stream-event handlers (tool calls, approvals,
//!   thinking blocks, floating-approval keyboard shortcuts).
//! - [`sub_agent`] — sub-agent progress trace and `add_info_message`.
//! - [`parent_stream`] — locate the parent assistant bubble when a
//!   sub-agent progress row is last.
//! - [`history`] — `load_history` (conversation switching).
//! - [`start_screen`] — onboarding / empty-state rendering.

#![allow(clippy::collapsible_if)]

mod handlers;
mod history;
mod parent_stream;
mod start_screen;
mod sub_agent;

use chatty_core::services::{AgentTaskSnapshot, AgentTodoStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use tracing::{debug, info, trace, warn};

use super::chat_input::{ChatInput, ChatInputState, ModelOption, slash_menu_items_with_skills};
use super::message_component::{DisplayMessage, MessageRenderCaches, MessageRole, render_message};
use super::message_types::SystemTrace;
use super::parsed_cache::{ParsedContentCache, StreamingParseState};
use super::thinking_indicator::{ThinkingIndicator, new_thinking_indicator};
use super::trace_components::SystemTraceView;
use super::transcript::{
    ApprovalCard, ArtifactMode, ArtifactView, ArtifactViewEvent, Block, OpenArtifact,
    PLAN_LIST_TOP_PADDING, PlanStrip, RunPin, RunPinKind, Turn, TurnRole,
    adapt_messages_with_traces, attach_plan_block, estimate_turn_height, format_worked_for,
    is_pdf_path, is_produced_file_tool, new_artifact_view, parse_unified_diff, plan_block_bottom,
    plan_is_above_viewport, plan_turn_index, read_artifact_source, render_typed_block,
    tool_file_path,
};
use crate::chatty::models::MessageFeedback;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelsModel;

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
    list_scroll: VirtualListScrollHandle,
    /// Explicit pin: auto-scroll only while this is false.
    user_scrolled_away: bool,
    /// Finished assistant turns fold unless the user expands them.
    collapsed_turns: HashMap<usize, bool>,
    pending_approval: Option<PendingApprovalInfo>,
    /// Tracks which tool calls are collapsed: (message_idx, tool_idx) -> collapsed
    collapsed_tool_calls: HashMap<(usize, usize), bool>,
    /// Tracks which diff views are fully expanded: (message_idx, tool_idx) -> expanded
    diff_expanded: HashMap<(usize, usize), bool>,
    /// Cache for parsed message content (markdown, math, code highlighting)
    parsed_cache: ParsedContentCache,
    /// Incremental streaming parse state, reusing stable content/markdown segments
    /// across streaming renders. Cleared on stream finalization or conversation switch.
    streaming_parse_cache: Option<StreamingParseState>,
    /// When true, every render re-asserts scroll_to_bottom so that async
    /// layout changes (image loading, SVG math, code blocks) never leave
    /// the view stuck above the true bottom. Disabled when user scrolls up.
    stick_to_bottom: bool,
    /// Keystroke interceptor that handles ↑/↓ for the slash-command picker.
    /// Must be held here so it stays alive (dropping it unregisters the handler).
    _slash_menu_interceptor: Subscription,
    /// Index into `messages` of the sub-agent progress row. Retained after
    /// the row is finalized so parent-stream updates skip it. `None` when
    /// this conversation has no progress row.
    sub_agent_progress_msg_idx: Option<usize>,
    /// Animated "Thinking…" indicator entity. Owns its own rotation
    /// timer so the spinner + label keep updating even when no stream
    /// events are arriving (typical while a tool runs silently).
    /// Reset on every new assistant message so the elapsed counter
    /// makes sense per-turn.
    thinking_indicator: Entity<ThinkingIndicator>,
    agent_task_snapshot: Option<AgentTaskSnapshot>,
    plan_overlay_open: bool,
    artifact_view: Entity<ArtifactView>,
    artifact_dismissed: bool,
    artifact_close_wired: bool,
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
        let list_scroll = VirtualListScrollHandle::new();

        // Subscribe to input events to handle Enter key
        let state_for_enter = chat_input_state.clone();
        let state_for_change = chat_input_state.clone();
        cx.subscribe(&input, move |_input_state, event: &InputEvent, cx| {
            match event {
                InputEvent::PressEnter { secondary } => {
                    // Only send on plain Enter (not Shift+Enter)
                    if !secondary {
                        tracing::debug!("Enter key pressed");
                        state_for_enter.update(cx, |state, cx| {
                            // If the slash-command menu is open, apply the selected
                            // command instead of sending the message as a chat turn.
                            if state.is_slash_menu_open(cx) {
                                state.apply_slash_command(cx);
                            } else if state.is_at_menu_open(cx) {
                                state.apply_at_mention(cx);
                            } else {
                                state.send_message(cx);
                            }
                        });
                    }
                }
                InputEvent::Change => {
                    // Reset the slash-menu selection when the query text changes,
                    // but NOT on spurious Change events with the same query (e.g.
                    // the newline that gpui-component writes before PressEnter).
                    state_for_change.update(cx, |state, cx| {
                        let new_text = state.input.read(cx).text().to_string();
                        state.reset_slash_menu_selection_if_query_changed(&new_text);
                        state.reset_at_menu_selection_if_query_changed(&new_text);

                        // Load files for the @ menu on first use.
                        let global_dir = cx
                            .try_global::<ExecutionSettingsModel>()
                            .and_then(|s| s.workspace_dir.clone())
                            .map(std::path::PathBuf::from)
                            .or_else(|| std::env::current_dir().ok());
                        if state.refresh_at_files_if_needed(&new_text, global_dir) {
                            cx.notify();
                        }
                    });
                }
                _ => {}
            }
        })
        .detach();

        // Focus the input immediately after creation
        chat_input_state.update(cx, |state, cx| {
            state.input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });

        // Register a keystroke interceptor to handle ↑/↓ navigation in the
        // slash-command picker and the @ mention picker.  This fires *before*
        // GPUI dispatches action handlers, so calling cx.stop_propagation()
        // here prevents the InputState's MoveUp/MoveDown cursor-movement
        // actions from running.
        let input_for_interceptor = chat_input_state.clone();
        let slash_menu_interceptor = cx.intercept_keystrokes(move |event, _window, cx| {
            let key = event.keystroke.key.as_str();
            // Only intercept plain ↑ / ↓ (no modifier keys).
            if (key != "up" && key != "down")
                || event.keystroke.modifiers.control
                || event.keystroke.modifiers.alt
                || event.keystroke.modifiers.platform
            {
                return;
            }
            // Check whether the slash-command picker is currently showing.
            let (input_text, skills) = {
                let state = input_for_interceptor.read(cx);
                (
                    state.input.read(cx).text().to_string(),
                    state.available_skills().to_vec(),
                )
            };
            let items = slash_menu_items_with_skills(&input_text, &skills);
            if !items.is_empty() {
                let num = items.len();
                input_for_interceptor.update(cx, |state, cx| {
                    if key == "up" {
                        state.move_slash_menu_up(num);
                    } else {
                        state.move_slash_menu_down(num);
                    }
                    cx.notify();
                });
                cx.stop_propagation();
                return;
            }
            // Then check @ mention picker.
            let at_items = input_for_interceptor
                .read(cx)
                .at_items_count_for_input(&input_text);
            if at_items > 0 {
                input_for_interceptor.update(cx, |state, cx| {
                    if key == "up" {
                        state.move_at_menu_up(at_items);
                    } else {
                        state.move_at_menu_down(at_items);
                    }
                    cx.notify();
                });
                cx.stop_propagation();
            }
        });

        Self {
            chat_input_state,
            messages: Vec::new(),
            conversation_id: None,
            scroll_handle,
            list_scroll,
            user_scrolled_away: false,
            collapsed_turns: HashMap::new(),
            pending_approval: None,
            collapsed_tool_calls: HashMap::new(),
            diff_expanded: HashMap::new(),
            parsed_cache: ParsedContentCache::new(),
            streaming_parse_cache: None,
            stick_to_bottom: true,
            _slash_menu_interceptor: slash_menu_interceptor,
            sub_agent_progress_msg_idx: None,
            thinking_indicator: new_thinking_indicator(cx),
            agent_task_snapshot: None,
            plan_overlay_open: false,
            artifact_view: new_artifact_view(cx),
            artifact_dismissed: false,
            artifact_close_wired: false,
        }
    }

    /// Get the chat input state entity (for wiring callbacks)
    pub fn chat_input_state(&self) -> &Entity<ChatInputState> {
        &self.chat_input_state
    }

    /// Get a reference to all displayed messages (for slash-command handlers, etc.).
    pub fn messages(&self) -> &[DisplayMessage] {
        &self.messages
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

    pub fn set_agent_task_snapshot(&mut self, snapshot: AgentTaskSnapshot, cx: &mut Context<Self>) {
        if snapshot.write_todos_called {
            self.agent_task_snapshot = Some(snapshot);
        } else {
            self.agent_task_snapshot = None;
            self.plan_overlay_open = false;
        }
        cx.notify();
    }

    pub fn clear_agent_task_snapshot(&mut self, cx: &mut Context<Self>) {
        self.agent_task_snapshot = None;
        self.plan_overlay_open = false;
        cx.notify();
    }

    fn plan_snapshot_active(&self) -> bool {
        self.agent_task_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.write_todos_called && !snapshot.todos.is_empty())
    }

    fn typed_turns(&self, cx: &App) -> Vec<Turn> {
        let collapsed: Vec<bool> = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, msg)| self.should_collapse_turn(index, msg))
            .collect();
        let traces = self.history_traces(cx);
        let mut turns = adapt_messages_with_traces(&self.messages, &collapsed, &traces);
        attach_plan_block(&mut turns, self.plan_snapshot_active());
        turns
    }

    fn should_collapse_turn(&self, index: usize, msg: &DisplayMessage) -> bool {
        if msg.is_streaming || !matches!(msg.role, MessageRole::Assistant) {
            return false;
        }
        self.collapsed_turns
            .get(&index)
            .copied()
            .unwrap_or_else(|| {
                msg.system_trace_view.is_some()
                    || msg
                        .live_trace
                        .as_ref()
                        .is_some_and(|trace| trace.has_items())
            })
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

        // Reset the thinking indicator so the elapsed counter restarts
        // and the user sees a fresh word for the new turn.
        self.thinking_indicator
            .update(cx, |indicator, cx| indicator.reset(cx));

        trace!(
            target: "chatty_gpui::render::stream",
            total_messages = self.messages.len(),
            conversation_id = ?self.conversation_id,
            "start_assistant_message",
        );
        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Append text to the current streaming assistant message
    pub fn append_assistant_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let idx = match self.parent_streaming_assistant_index() {
            Some(idx) => idx,
            None if self.sub_agent_progress_msg_idx.is_some() => {
                // Progress row is last; open a continuation bubble below it.
                self.start_assistant_message(cx);
                self.messages.len() - 1
            }
            None => {
                warn!(
                    target: "chatty_gpui::render::stream",
                    delta_len = text.len(),
                    "append_assistant_text dropped: no parent streaming assistant",
                );
                return;
            }
        };
        let content_len_before = self.messages[idx].content.len();
        self.messages[idx].content.push_str(text);
        trace!(
            target: "chatty_gpui::render::stream",
            delta_len = text.len(),
            content_len_before,
            new_content_len = self.messages[idx].content.len(),
            message_idx = idx,
            conversation_id = ?self.conversation_id,
            "append_assistant_text",
        );
        cx.notify();
        self.scroll_if_sticky();
    }

    /// Finalize the current streaming assistant message
    pub fn finalize_assistant_message(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.parent_streaming_assistant_index() else {
            return;
        };

        let empty = self.messages[idx].content.is_empty()
            && !self.messages[idx]
                .live_trace
                .as_ref()
                .is_some_and(|t| t.has_items())
            && self.messages[idx].system_trace_view.is_none();
        if empty {
            self.messages.remove(idx);
            self.streaming_parse_cache = None;
            cx.notify();
            return;
        }

        let last = &mut self.messages[idx];
        let had_live_trace = last.live_trace.is_some();
        let had_streaming_cache = self.streaming_parse_cache.is_some();
        let content_len = last.content.len();

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

        trace!(
            target: "chatty_gpui::render::stream",
            had_live_trace,
            cleared_streaming_cache = had_streaming_cache,
            content_len,
            conversation_id = ?self.conversation_id,
            "finalize_assistant_message",
        );

        cx.notify();
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
        if let Some(idx) = self.parent_assistant_index() {
            self.messages[idx].history_index = Some(history_index);
            cx.notify();
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
        if let Some(idx) = self.parent_assistant_index() {
            self.messages[idx].attachments = attachments;
            cx.notify();
        }
    }

    /// Mark the current streaming message as cancelled by the user
    pub fn mark_message_cancelled(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.parent_streaming_assistant_index() else {
            return;
        };
        let last = &mut self.messages[idx];
        // Append cancellation notice to the message
        if !last.content.is_empty() {
            last.content.push_str("\n\n");
        }
        last.content.push_str("*[Response cancelled by user]*");
        last.is_streaming = false;

        // Clear streaming parse cache
        self.streaming_parse_cache = None;

        // Finalize trace if present: cancel all Running tool calls
        // so they don't stay stuck in the Running state permanently
        if let Some(ref mut trace) = last.live_trace {
            trace.cancel_running_tool_calls();
            trace.clear_active_tool();

            // Update the SystemTraceView with the final cancelled state
            let trace_clone = trace.clone();
            if let Some(ref view_entity) = last.system_trace_view {
                view_entity.update(cx, |view, cx| {
                    view.update_trace(trace_clone, cx);
                });
            }
        }
        last.live_trace = None;

        cx.notify();
    }

    /// Extract the current trace before finalizing (for persistence)
    pub fn extract_current_trace(&mut self) -> Option<SystemTrace> {
        // Prefer a trace that actually has items. The continuation bubble
        // below a sub-agent card starts with an empty live_trace and must
        // not hide the pre-tool parent's tool calls (or the Conversation
        // model's streaming_trace fallback).
        for i in (0..self.messages.len()).rev() {
            if Some(i) == self.sub_agent_progress_msg_idx {
                continue;
            }
            if let Some(ref mut trace) = self.messages[i].live_trace {
                if trace.has_items() {
                    trace.clear_active_tool();
                    return Some(trace.clone());
                }
            }
        }
        None
    }

    /// Restore a live trace from a saved SystemTrace (e.g. when switching back to a streaming conversation).
    /// Creates the SystemTraceView entity and subscribes to its events.
    pub fn restore_live_trace(&mut self, trace: SystemTrace, cx: &mut Context<Self>) {
        let Some(idx) = self.parent_streaming_assistant_index() else {
            return;
        };
        let last = &mut self.messages[idx];

        last.live_trace = Some(trace.clone());

        if trace.has_items() {
            let trace_view = cx.new(|_cx| SystemTraceView::new(trace));

            let chat_view_entity = cx.entity();
            cx.subscribe(
                &trace_view,
                move |_chat_view, _trace_view, event: &super::message_types::TraceEvent, cx| {
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
        }

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
        self.user_scrolled_away = false;
        self.list_scroll.scroll_to_bottom();
        self.scroll_handle.scroll_to_bottom();
    }

    /// If sticky-scroll is active, re-assert scroll_to_bottom for this frame.
    /// Used for incremental streaming updates — respects the user's decision
    /// to scroll up by not re-enabling sticky mode.
    fn scroll_if_sticky(&mut self) {
        if self.stick_to_bottom && !self.user_scrolled_away {
            self.list_scroll.scroll_to_bottom();
            self.scroll_handle.scroll_to_bottom();
        }
    }

    /// Index of the parent streaming assistant bubble.
    ///
    /// Skips the dedicated sub-agent progress row, which is pushed *after*
    /// the parent bubble and would otherwise steal `messages.last()`.
    pub(super) fn parent_streaming_assistant_index(&self) -> Option<usize> {
        parent_stream::index_of_parent_streaming_assistant(
            &self.messages,
            self.sub_agent_progress_msg_idx,
        )
    }

    /// Last assistant bubble that is not the sub-agent progress row.
    /// Used after the parent stream has already been finalized.
    pub(super) fn parent_assistant_index(&self) -> Option<usize> {
        parent_stream::index_of_parent_assistant(&self.messages, self.sub_agent_progress_msg_idx)
    }

    pub(super) fn parent_streaming_message_mut(&mut self) -> Option<&mut DisplayMessage> {
        let idx = self.parent_streaming_assistant_index()?;
        self.messages.get_mut(idx)
    }

    /// Check if we're awaiting a response (streaming message with no content yet
    /// and no tool calls in progress)
    fn is_awaiting_response(&self) -> bool {
        self.parent_streaming_assistant_index()
            .and_then(|i| self.messages.get(i))
            .is_some_and(|msg| {
                msg.content.is_empty()
                    && !msg
                        .live_trace
                        .as_ref()
                        .is_some_and(|trace| trace.has_items())
            })
    }

    /// Whether to show the animated "thinking" indicator at the bottom
    /// of the message list. We show it whenever any assistant message is
    /// still streaming (parent bubble or in-flight progress card). This
    /// matches Claude Code / Cursor behaviour: a continuous "agent is
    /// working" signal until the stream actually ends.
    fn is_thinking_indicator_visible(&self) -> bool {
        self.messages
            .iter()
            .any(|msg| matches!(msg.role, MessageRole::Assistant) && msg.is_streaming)
    }

    fn running_step_progress(&self) -> (usize, usize) {
        if let Some(snapshot) = self
            .agent_task_snapshot
            .as_ref()
            .filter(|snap| snap.write_todos_called && !snap.todos.is_empty())
        {
            let done = snapshot
                .todos
                .iter()
                .filter(|todo| matches!(todo.status, AgentTodoStatus::Done))
                .count();
            return (done, snapshot.todos.len());
        }
        let tools: Vec<_> = self
            .messages
            .iter()
            .filter_map(|msg| msg.live_trace.as_ref())
            .flat_map(|trace| trace.items.iter())
            .filter_map(|item| match item {
                crate::chatty::views::message_types::TraceItem::ToolCall(tool) => Some(tool),
                _ => None,
            })
            .collect();
        if tools.is_empty() {
            return (0, 0);
        }
        let done = tools
            .iter()
            .filter(|tool| {
                !matches!(
                    tool.state,
                    crate::chatty::views::message_types::ToolCallState::Running
                )
            })
            .count();
        (done, tools.len())
    }

    fn last_file_artifact(&self, cx: &App) -> Option<(PathBuf, String)> {
        let traces = self.history_traces(cx);
        for (msg, hist) in self.messages.iter().zip(traces.iter()).rev() {
            let Some(trace) = msg.live_trace.as_ref().or(hist.as_ref()) else {
                continue;
            };
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if let Some(parsed) = parse_unified_diff(tool.output.as_deref().unwrap_or(""))
                    && !parsed.path.is_empty()
                {
                    let path = PathBuf::from(parsed.path);
                    let source = if is_pdf_path(&path) || parsed.new.trim().is_empty() {
                        read_artifact_source(&path)
                    } else {
                        parsed.new
                    };
                    return Some((path, source));
                }
                if is_produced_file_tool(&tool.tool_name, &tool.input)
                    && let Some(path) = tool_file_path(&tool.input)
                {
                    return Some((path.clone(), read_artifact_source(&path)));
                }
            }
        }
        None
    }

    fn maybe_open_artifact_panel(&mut self, cx: &mut Context<Self>) {
        if !self.artifact_close_wired {
            self.artifact_close_wired = true;
            cx.subscribe(&self.artifact_view, |this, _, _: &ArtifactViewEvent, cx| {
                this.artifact_dismissed = true;
                cx.notify();
            })
            .detach();
        }
        if self.artifact_dismissed {
            return;
        }
        if self.artifact_view.read(cx).mode != ArtifactMode::Closed {
            return;
        }
        let Some((path, source)) = self.last_file_artifact(cx) else {
            return;
        };
        self.artifact_view
            .update(cx, |view, cx| view.open(path, source, cx));
    }

    fn show_artifact(&mut self, path: PathBuf, source: String, cx: &mut Context<Self>) {
        self.artifact_dismissed = false;
        self.artifact_view
            .update(cx, |view, cx| view.open(path, source, cx));
        cx.notify();
    }

    fn history_traces(&self, cx: &App) -> Vec<Option<SystemTrace>> {
        self.messages
            .iter()
            .map(|msg| {
                msg.live_trace.clone().or_else(|| {
                    msg.system_trace_view.as_ref().and_then(|view| {
                        let trace = view.read(cx).get_trace().clone();
                        trace.has_items().then_some(trace)
                    })
                })
            })
            .collect()
    }

    /// Pre-render side effects: sticky scroll, input clearing, model refresh.
    fn prepare_render(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Sticky-scroll: re-assert scroll_to_bottom on every render so that
        // async layout changes (image loading, SVG math, code blocks) always
        // converge to the true bottom. Detect user scroll-away to disable.
        if self.stick_to_bottom && !self.user_scrolled_away {
            let offset = self.list_scroll.offset();
            let max_offset = self.list_scroll.max_offset();
            let distance_from_bottom = max_offset.height + offset.y;

            if distance_from_bottom > px(10.0) && max_offset.height > px(0.0) {
                self.stick_to_bottom = false;
                self.user_scrolled_away = true;
                trace!(
                    distance = %distance_from_bottom,
                    "Sticky scroll disabled: user scrolled up"
                );
            } else {
                self.list_scroll.scroll_to_bottom();
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
                }
            }
        }

        // Refresh available models from global store (in case they changed)
        if let Some(models_model) = cx.try_global::<ModelsModel>() {
            let models_list: Vec<ModelOption> = models_model
                .models()
                .iter()
                .map(|m| ModelOption::new(m.id.clone(), m.name.clone(), m.provider_type.clone()))
                .collect();

            if !models_list.is_empty() {
                self.chat_input_state.update(cx, |state, _cx| {
                    if state.available_models().is_empty()
                        || state.available_models() != models_list.as_slice()
                    {
                        let default_model_id = models_list.first().map(|model| model.id.clone());
                        state.set_available_models(models_list, default_model_id);
                    }
                });
            }
        }
    }

    /// Render the scrollable message list area including the loading skeleton.
    fn render_message_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_awaiting = self.is_awaiting_response();
        let turns = self.typed_turns(cx);
        let show_start_screen = turns.is_empty() && !is_awaiting;
        let thinking_visible = self.is_thinking_indicator_visible();
        if thinking_visible {
            let attention = self.messages.iter().rev().find_map(|msg| {
                msg.live_trace.as_ref().and_then(|trace| {
                    trace.items.iter().rev().find_map(|item| match item {
                        crate::chatty::views::message_types::TraceItem::ToolCall(tool) => {
                            Some(if tool.display_name.is_empty() {
                                tool.tool_name.clone()
                            } else {
                                tool.display_name.clone()
                            })
                        }
                        _ => None,
                    })
                })
            });
            let (steps_done, steps_total) = self.running_step_progress();
            self.thinking_indicator.update(cx, |indicator, cx| {
                if let Some(attention) = attention {
                    indicator.set_attention(attention, cx);
                }
                indicator.set_progress(steps_done, steps_total, cx);
            });
        }
        let thinking_indicator = self.thinking_indicator.clone();
        let plan_steps = self
            .agent_task_snapshot
            .as_ref()
            .map(|snapshot| snapshot.todos.len())
            .unwrap_or(0);
        let sizes = Rc::new(
            turns
                .iter()
                .map(|turn| estimate_turn_height(turn, plan_steps))
                .collect::<Vec<_>>(),
        );
        let entity = cx.entity();
        let user_away = self.user_scrolled_away;
        let has_approval = self.active_approval_for_display().is_some();
        let has_plan = self.plan_snapshot_active();
        let show_strip = has_plan
            && self.list_scroll.max_offset().height > px(0.0)
            && plan_block_bottom(&turns, plan_steps, px(16.0))
                .is_some_and(|bottom| plan_is_above_viewport(bottom, -self.list_scroll.offset().y));
        if !show_strip {
            self.plan_overlay_open = false;
        }
        let plan_strip = self.agent_task_snapshot.clone().filter(|_| show_strip);
        let jump_turn = plan_turn_index(&turns);
        let jump_message = jump_turn.and_then(|ix| turns.get(ix).map(|turn| turn.message_index));
        let overlay_open = self.plan_overlay_open;

        trace!(
            target: "chatty_gpui::render::list",
            total = self.messages.len(),
            visible = turns.len(),
            is_awaiting = is_awaiting,
            thinking_visible = thinking_visible,
            conversation_id = ?self.conversation_id,
            "render_message_list",
        );

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .when_some(plan_strip, |this, snapshot| {
                this.child(
                    div()
                        .id("plan-strip-slot")
                        .h(px(PLAN_LIST_TOP_PADDING))
                        .w_full()
                        .px_4()
                        .flex()
                        .items_center()
                        .child(
                            PlanStrip::new(snapshot)
                                .open(overlay_open)
                                .on_open_change({
                                    let entity = entity.clone();
                                    move |open, cx| {
                                        entity.update(cx, |view, cx| {
                                            view.plan_overlay_open = open;
                                            cx.notify();
                                        });
                                    }
                                })
                                .on_jump({
                                    let entity = entity.clone();
                                    move |cx| {
                                        entity.update(cx, |view, cx| {
                                            if let Some(msg_index) = jump_message {
                                                view.collapsed_turns.insert(msg_index, false);
                                            }
                                            view.plan_overlay_open = false;
                                            if let Some(turn_ix) = jump_turn {
                                                view.list_scroll
                                                    .scroll_to_item(turn_ix, ScrollStrategy::Top);
                                            }
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                )
            })
            .when(show_start_screen, |this| {
                this.child(
                    div()
                        .flex_1()
                        .h_full()
                        .items_center()
                        .justify_center()
                        .child(self.render_start_screen(cx)),
                )
            })
            .when(!show_start_screen, |this| {
                this.child(
                    div()
                        .id("transcript-scroll")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            v_virtual_list(
                                entity,
                                "transcript",
                                sizes,
                                move |this, range, window, cx| {
                                    this.render_visible_turns(range, window, cx)
                                },
                            )
                            .track_scroll(&self.list_scroll)
                            .p_4()
                            .flex_1(),
                        )
                        .when(overlay_open, |this| {
                            this.child(
                                div()
                                    .id("plan-overlay-scrim")
                                    .absolute()
                                    .inset_0()
                                    .bg(cx.theme().overlay),
                            )
                        }),
                )
                .vertical_scrollbar(&self.list_scroll)
            })
            .when(thinking_visible, |this| this.child(thinking_indicator))
            .child(
                RunPin::new(if has_approval && user_away {
                    RunPinKind::PendingApproval
                } else {
                    RunPinKind::JumpToLatest
                })
                .visible(user_away)
                .on_click({
                    let entity = cx.entity();
                    move |cx| {
                        entity.update(cx, |view, cx| {
                            view.activate_sticky_scroll();
                            cx.notify();
                        });
                    }
                }),
            )
    }

    fn render_visible_turns(
        &mut self,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let turns = self.typed_turns(cx);
        let last_visible_assistant_idx = turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.role, TurnRole::Assistant) && !turn.streaming)
            .map(|turn| turn.message_index);

        let entity = cx.entity();
        let plan = self.agent_task_snapshot.clone();
        range
            .filter_map(|ix| turns.get(ix).cloned())
            .map(|turn| {
                if turn.collapsed {
                    let label = format_worked_for(turn.elapsed);
                    let msg_index = turn.message_index;
                    let entity = entity.clone();
                    return div()
                        .id(ElementId::NamedInteger("turn-collapsed".into(), turn.id))
                        .h(px(super::transcript::COLLAPSED_TURN_HEIGHT))
                        .flex()
                        .items_center()
                        .px_3()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            entity.update(cx, |view, cx| {
                                view.collapsed_turns.insert(msg_index, false);
                                cx.notify();
                            });
                        })
                        .child(label)
                        .into_any_element();
                }

                let Some(msg) = self.messages.get(turn.message_index) else {
                    return div().into_any_element();
                };
                let history_index = msg.history_index;
                let is_last_message = last_visible_assistant_idx == Some(turn.message_index);
                let entity_clone = entity.clone();
                let entity_for_diff = entity.clone();
                let entity_for_feedback = entity.clone();
                let entity_for_regenerate = entity.clone();
                let mut streaming_slot = if msg.is_streaming {
                    self.streaming_parse_cache.take()
                } else {
                    None
                };
                let on_open: OpenArtifact = {
                    let entity = entity.clone();
                    Rc::new(move |path, source, cx| {
                        entity.update(cx, |view, cx| {
                            view.show_artifact(path, source, cx);
                        });
                    })
                };
                let typed: Vec<AnyElement> = turn
                    .blocks
                    .iter()
                    .filter(|block| !matches!(block, Block::User { .. } | Block::Text { .. }))
                    .map(|block| {
                        render_typed_block(block, Some(on_open.clone()), plan.as_ref(), _window, cx)
                    })
                    .collect();
                let mut msg_for_text = msg.clone();
                if !typed.is_empty() {
                    msg_for_text.system_trace_view = None;
                }
                let text = render_message(
                    &msg_for_text,
                    turn.message_index,
                    is_last_message,
                    &self.collapsed_tool_calls,
                    &self.diff_expanded,
                    &mut MessageRenderCaches {
                        parsed: &mut self.parsed_cache,
                        streaming: &mut streaming_slot,
                    },
                    move |msg_idx, tool_idx, cx| {
                        entity_clone.update(cx, |chat_view, cx| {
                            let key = (msg_idx, tool_idx);
                            let current = chat_view
                                .collapsed_tool_calls
                                .get(&key)
                                .copied()
                                .unwrap_or(true);
                            chat_view.collapsed_tool_calls.insert(key, !current);
                            cx.notify();
                        });
                    },
                    move |msg_idx, tool_idx, cx| {
                        entity_for_diff.update(cx, |chat_view, cx| {
                            let key = (msg_idx, tool_idx);
                            let current =
                                chat_view.diff_expanded.get(&key).copied().unwrap_or(false);
                            chat_view.diff_expanded.insert(key, !current);
                            cx.notify();
                        });
                    },
                    move |msg_idx, feedback, cx| {
                        entity_for_feedback.update(cx, |chat_view, cx| {
                            if let Some(display_msg) = chat_view.messages.get_mut(msg_idx) {
                                display_msg.feedback = feedback.clone();
                            }
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
                );
                if streaming_slot.is_some() {
                    self.streaming_parse_cache = streaming_slot;
                }
                if typed.is_empty() {
                    text
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .w_full()
                        .children(typed)
                        .child(text)
                        .into_any_element()
                }
            })
            .collect()
    }

    /// Return the pending approval if it belongs to the current conversation.
    fn active_approval_for_display(&self) -> Option<PendingApprovalInfo> {
        self.pending_approval
            .as_ref()
            .filter(|approval| self.conversation_id.as_ref() == Some(&approval.conversation_id))
            .cloned()
    }

    /// Render the `CHATTY_DEBUG_UI` overlay (top-right of the chat pane) when
    /// the env var is set at process start. Lists per-message render state so
    /// rendering bugs can be diagnosed live without grepping logs.
    ///
    /// See [`docs/debug_ui.md`](../../../../../../docs/debug_ui.md) for the
    /// field legend.
    fn render_debug_overlay(&self, cx: &App) -> Option<AnyElement> {
        if !*DEBUG_UI_ENABLED {
            return None;
        }

        let total = self.messages.len();
        let visible = self
            .messages
            .iter()
            .filter(|msg| {
                !(msg.is_streaming
                    && msg.content.is_empty()
                    && !msg
                        .live_trace
                        .as_ref()
                        .is_some_and(|trace| trace.has_items()))
            })
            .count();
        let filtered = total - visible;
        let is_awaiting = self.is_awaiting_response();

        let header = format!(
            "ChatView debug\n  msgs: {visible} visible / {total} total   awaiting: {is_awaiting}   skeleton: {is_awaiting}   filtered: {filtered}"
        );

        let mut lines: Vec<String> = vec![header];
        for (idx, msg) in self.messages.iter().enumerate() {
            let role = match msg.role {
                MessageRole::User => "User     ",
                MessageRole::Assistant => "Assistant",
            };
            let trace_items = msg
                .live_trace
                .as_ref()
                .map(|t| t.items.len())
                .or_else(|| {
                    msg.system_trace_view
                        .as_ref()
                        .map(|v| v.read(cx).get_trace().items.len())
                })
                .unwrap_or(0);
            let trace_state = if let Some(view) = msg.system_trace_view.as_ref() {
                // Note: `is_collapsed` is private; infer from existence + items.
                let _ = view;
                if trace_items > 0 { "open" } else { "empty" }
            } else if msg.live_trace.is_some() {
                "live"
            } else {
                "none"
            };
            lines.push(format!(
                "  [{idx}] {role}  s={} m={} ti={} c={}  trace={}",
                msg.is_streaming as u8,
                msg.is_markdown as u8,
                trace_items,
                msg.content.len(),
                trace_state,
            ));
        }

        Some(
            div()
                .absolute()
                .top_2()
                .right_2()
                .p_2()
                .rounded_md()
                .bg(gpui::black().opacity(0.7))
                .text_color(gpui::white())
                .text_xs()
                .child(lines.join("\n"))
                .into_any_element(),
        )
    }
}

/// Process-wide flag for the `CHATTY_DEBUG_UI` env var. Read once at startup
/// so each render call is a single atomic load rather than a syscall.
static DEBUG_UI_ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    std::env::var("CHATTY_DEBUG_UI")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
});

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_render(window, cx);
        self.maybe_open_artifact_panel(cx);
        let docked = self.artifact_view.read(cx).mode == ArtifactMode::Docked;
        let artifact = self.artifact_view.clone();

        let has_pending_approval = self.pending_approval.is_some();
        let view_entity_for_keys = cx.entity();
        let pending_conv_id = self
            .pending_approval
            .as_ref()
            .map(|p| p.conversation_id.clone());
        let current_conv_id = self.conversation_id.clone();

        let column = div()
            .flex_1()
            .h_full()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .when(cfg!(target_os = "macos"), |this| this.pt(px(24.)))
            .when(has_pending_approval, |this| {
                this.on_key_down(move |event: &KeyDownEvent, _window, cx| {
                    let modifiers = event.keystroke.modifiers;
                    let key = &event.keystroke.key;

                    warn!(
                        "ChatView key down: key={}, platform={}",
                        key, modifiers.platform
                    );

                    if pending_conv_id.as_ref() != current_conv_id.as_ref() {
                        warn!(
                            "Ignoring keyboard shortcut: approval belongs to different conversation (pending: {:?}, current: {:?})",
                            pending_conv_id, current_conv_id
                        );
                        return;
                    }

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
            .child(self.render_message_list(cx))
            .when_some(self.render_debug_overlay(cx), |this, overlay| {
                this.child(overlay)
            })
            .when_some(self.active_approval_for_display(), |this, pending| {
                let view_entity = cx.entity();
                let approval = chatty_core::models::message_types::ApprovalBlock {
                    id: pending.id,
                    command: pending.command,
                    is_sandboxed: pending.is_sandboxed,
                    state: chatty_core::models::message_types::ApprovalState::Pending,
                    created_at: std::time::SystemTime::now(),
                };
                this.child(
                    div().px_4().child(
                        ApprovalCard::new(approval).on_decide({
                            let entity = view_entity.clone();
                            move |approved, cx| {
                                entity.update(cx, |view, cx| {
                                    view.handle_floating_approval(approved, cx);
                                });
                            }
                        }),
                    ),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .w_full()
                    .px_4()
                    .pt_2()
                    .pb_4()
                    .child({
                        ChatInput::new(self.chat_input_state.clone()).into_any_element()
                    }),
            );

        div()
            .flex_1()
            .h_full()
            .w_full()
            .flex()
            .flex_row()
            .bg(cx.theme().background)
            .overflow_hidden()
            .child(column)
            .when(docked, |this| this.child(artifact.into_any_element()))
    }
}
