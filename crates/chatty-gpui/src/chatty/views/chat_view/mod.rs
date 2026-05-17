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
//! - [`history`] — `load_history` (conversation switching).
//! - [`start_screen`] — onboarding / empty-state rendering.

#![allow(clippy::collapsible_if)]

mod handlers;
mod history;
mod start_screen;
mod sub_agent;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, trace, warn};

use super::chat_input::{ChatInput, ChatInputState, ModelOption, slash_menu_items_with_skills};
use super::message_component::{DisplayMessage, MessageRenderCaches, MessageRole, render_message};
use super::message_types::SystemTrace;
use super::parsed_cache::{ParsedContentCache, StreamingParseState};
use super::thinking_indicator::{ThinkingIndicator, new_thinking_indicator};
use super::trace_components::SystemTraceView;
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
    /// Index into `messages` of the "Sub-agent: launching…" info message that
    /// receives live progress lines while a sub-agent subprocess is running.
    /// `None` when no sub-agent is active.
    sub_agent_progress_msg_idx: Option<usize>,
    /// Animated "Thinking…" indicator entity. Owns its own rotation
    /// timer so the spinner + label keep updating even when no stream
    /// events are arriving (typical while a tool runs silently).
    /// Reset on every new assistant message so the elapsed counter
    /// makes sense per-turn.
    thinking_indicator: Entity<ThinkingIndicator>,
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
            pending_approval: None,
            collapsed_tool_calls: HashMap::new(),
            diff_expanded: HashMap::new(),
            parsed_cache: ParsedContentCache::new(),
            streaming_parse_cache: None,
            stick_to_bottom: true,
            _slash_menu_interceptor: slash_menu_interceptor,
            sub_agent_progress_msg_idx: None,
            thinking_indicator: new_thinking_indicator(cx),
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
        let last_msg_streaming = self
            .messages
            .last()
            .map(|m| m.is_streaming)
            .unwrap_or(false);
        let content_len_before = self.messages.last().map(|m| m.content.len()).unwrap_or(0);

        if let Some(last) = self.messages.last_mut() {
            if last.is_streaming {
                last.content.push_str(text);
                trace!(
                    target: "chatty_gpui::render::stream",
                    delta_len = text.len(),
                    content_len_before,
                    new_content_len = last.content.len(),
                    last_msg_streaming,
                    conversation_id = ?self.conversation_id,
                    "append_assistant_text",
                );
                cx.notify();
                self.scroll_if_sticky();
            } else {
                warn!(
                    target: "chatty_gpui::render::stream",
                    delta_len = text.len(),
                    "append_assistant_text dropped: last message not streaming",
                );
            }
        } else {
            warn!(
                target: "chatty_gpui::render::stream",
                delta_len = text.len(),
                "append_assistant_text dropped: no messages in view",
            );
        }
    }

    /// Finalize the current streaming assistant message
    pub fn finalize_assistant_message(&mut self, cx: &mut Context<Self>) {
        if let Some(last) = self.messages.last_mut() {
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
        } else {
            warn!(
                target: "chatty_gpui::render::stream",
                "finalize_assistant_message called with no messages",
            );
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

    /// Restore a live trace from a saved SystemTrace (e.g. when switching back to a streaming conversation).
    /// Creates the SystemTraceView entity and subscribes to its events.
    pub fn restore_live_trace(&mut self, trace: SystemTrace, cx: &mut Context<Self>) {
        let last = match self.messages.last_mut() {
            Some(msg) if msg.is_streaming => msg,
            _ => return,
        };

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

    /// Whether to show the animated "thinking" indicator at the bottom
    /// of the message list. We show it whenever the last assistant
    /// message is still streaming, regardless of whether text or tool
    /// chunks have already arrived. This matches Claude Code / Cursor
    /// behaviour: a continuous "agent is working" signal until the
    /// stream actually ends, so the user never sees a silent gap
    /// between text chunks, between tool calls, or while a tool runs.
    fn is_thinking_indicator_visible(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|msg| matches!(msg.role, MessageRole::Assistant) && msg.is_streaming)
    }

    /// Pre-render side effects: sticky scroll, input clearing, model refresh.
    fn prepare_render(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Sticky-scroll: re-assert scroll_to_bottom on every render so that
        // async layout changes (image loading, SVG math, code blocks) always
        // converge to the true bottom. Detect user scroll-away to disable.
        if self.stick_to_bottom {
            let offset = self.scroll_handle.offset();
            let max_offset = self.scroll_handle.max_offset();
            let distance_from_bottom = max_offset.height + offset.y;

            if distance_from_bottom > px(10.0) && max_offset.height > px(0.0) {
                self.stick_to_bottom = false;
                trace!(
                    distance = %distance_from_bottom,
                    "Sticky scroll disabled: user scrolled up"
                );
            } else {
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
        let chat_view_entity = cx.entity();

        // Temporarily move state out to avoid split borrows
        let collapsed_tool_calls = std::mem::take(&mut self.collapsed_tool_calls);
        let diff_expanded = std::mem::take(&mut self.diff_expanded);
        let mut parsed_cache = std::mem::take(&mut self.parsed_cache);
        let mut streaming_cache = self.streaming_parse_cache.take();

        let total_messages = self.messages.len();
        let visible_messages: Vec<(usize, &DisplayMessage)> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| {
                !(msg.is_streaming
                    && msg.content.is_empty()
                    && !msg
                        .live_trace
                        .as_ref()
                        .is_some_and(|trace| trace.has_items()))
            })
            .collect();

        trace!(
            target: "chatty_gpui::render::list",
            total = total_messages,
            visible = visible_messages.len(),
            filtered = total_messages - visible_messages.len(),
            is_awaiting = is_awaiting,
            thinking_visible = is_awaiting,
            conversation_id = ?self.conversation_id,
            "render_message_list",
        );

        let last_visible_assistant_idx = visible_messages
            .iter()
            .rev()
            .find(|(_, msg)| {
                matches!(msg.role, MessageRole::Assistant)
                    && !msg.is_streaming
                    && msg.live_trace.is_none()
            })
            .map(|(idx, _)| *idx);

        let mut rendered: Vec<AnyElement> = visible_messages
            .into_iter()
            .map(|(index, msg)| {
                let entity_clone = chat_view_entity.clone();
                let entity_for_diff = chat_view_entity.clone();
                let entity_for_feedback = chat_view_entity.clone();
                let entity_for_regenerate = chat_view_entity.clone();
                let history_index = msg.history_index;
                let is_last_message = last_visible_assistant_idx == Some(index);
                let mut no_cache: Option<StreamingParseState> = None;
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
                    &diff_expanded,
                    &mut MessageRenderCaches {
                        parsed: &mut parsed_cache,
                        streaming: sc,
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
                )
                .into_any_element()
            })
            .collect();

        let show_start_screen = rendered.is_empty() && !is_awaiting;
        if show_start_screen {
            rendered.push(self.render_start_screen(cx).into_any_element());
        }

        // Move state back
        self.parsed_cache = parsed_cache;
        self.streaming_parse_cache = streaming_cache;
        self.collapsed_tool_calls = collapsed_tool_calls;
        self.diff_expanded = diff_expanded;

        let thinking_visible = self.is_thinking_indicator_visible();
        let thinking_indicator = self.thinking_indicator.clone();

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                div()
                    .id("chat-messages")
                    .track_scroll(&self.scroll_handle)
                    .overflow_scroll()
                    .size_full()
                    .child(
                        div()
                            .p_4()
                            .w_full()
                            .flex()
                            .flex_col()
                            .when(show_start_screen, |this| {
                                this.h_full().items_center().justify_center().gap_0()
                            })
                            .when(!show_start_screen, |this| this.gap_4())
                            .children(rendered)
                            .when(thinking_visible, |this| this.child(thinking_indicator)),
                    ),
            )
            .vertical_scrollbar(&self.scroll_handle)
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
            .relative()
            .bg(cx.theme().background)
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
                        }),
                    ),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .p_4()
                    .child(ChatInput::new(self.chat_input_state.clone())),
            )
    }
}
