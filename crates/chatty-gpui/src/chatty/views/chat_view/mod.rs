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
mod scroll;
mod start_screen;
mod sub_agent;

use chatty_core::models::clarification_store::{ClarifyingQuestion, MAX_CLARIFYING_QUESTIONS};
use chatty_core::services::{AgentTaskSnapshot, AgentTodoStatus};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Icon, IconName};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

use super::chat_input::{ChatInput, ChatInputState, ModelOption, slash_menu_items_with_skills};
use super::message_component::{DisplayMessage, MessageRenderCaches, MessageRole, render_message};
use super::message_types::SystemTrace;
use super::parsed_cache::{ParsedContentCache, StreamingParseState};
use super::thinking_indicator::{ThinkingIndicator, new_thinking_indicator};
use super::trace_components::SystemTraceView;
use super::transcript::{
    ApprovalCard, ArtifactMode, ArtifactOpen, ArtifactView, ArtifactViewEvent, Block, ChosenOption,
    ClarificationCard, FileChange, OpenArtifact, OpenTable, PLAN_LIST_TOP_PADDING, PlanStrip,
    RunPin, RunPinKind, SessionChangeBar, TableOpen, Turn, TurnFileOverview, TurnRole,
    adapt_messages_with_traces, attach_plan_block, attachment_image_path, block_visible_in_turn,
    extract_table_preview, file_change_from_tool, file_changes_from_turn, format_worked_for,
    format_working_for, is_lane_a_browser_tool, is_pdf_artifact_tool, is_pdf_path,
    merge_file_changes, new_artifact_view, plan_turn_index, produced_path_is_openable,
    read_artifact_source, render_typed_block, resolve_artifact_path, tool_file_path,
    turn_has_work_fold,
};
use crate::chatty::models::{GlobalStreamManager, MessageFeedback};
use crate::chatty::views::chart_renderer::extract_chart_spec;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::general_model::GeneralSettingsModel;
use crate::settings::models::models_store::ModelsModel;

/// Main chat view component
#[derive(Clone)]
pub struct PendingApprovalInfo {
    pub id: String,
    pub command: String,
    pub is_sandboxed: bool,
    pub conversation_id: String,
}

/// A clarification request the agent is currently blocked on.
pub struct PendingClarificationInfo {
    pub id: String,
    pub conversation_id: String,
    pub questions: Vec<ClarifyingQuestion>,
    /// Options the user has clicked so far, keyed by question id. A question
    /// with free text typed into its box does not need an entry here.
    pub choices: HashMap<String, ChosenOption>,
}

/// Snapshot of the pending clarification handed to the card each render.
struct PendingClarificationDisplay {
    id: String,
    questions: Vec<ClarifyingQuestion>,
    choices: HashMap<String, ChosenOption>,
}

pub struct ChatView {
    chat_input_state: Entity<ChatInputState>,
    messages: Vec<DisplayMessage>,
    conversation_id: Option<String>,
    /// Transcript list state. Owns the measured height of every turn it has
    /// laid out, so nothing in this view predicts a height.
    transcript_list: ListState,
    /// Per-turn content fingerprints from the last frame, to spot which turns
    /// changed shape and need re-measuring.
    transcript_fingerprints: Vec<u64>,
    /// Font size the list's cached heights were measured at. `List` drops its
    /// cache when the list's *width* changes, but not when the font does.
    last_font_size: f32,
    /// Explicit pin: auto-scroll only while this is false.
    user_scrolled_away: bool,
    /// Finished assistant turns fold unless the user expands them.
    collapsed_turns: HashMap<usize, bool>,
    pending_approval: Option<PendingApprovalInfo>,
    pending_clarification: Option<PendingClarificationInfo>,
    /// Free-text answer boxes for the clarification popover, one per question
    /// slot. Built up front because `InputState::new` needs a `Window`, which
    /// the stream-event handler that receives the questions does not have.
    clarification_inputs: Vec<Entity<InputState>>,
    /// Set when a new clarification arrives; the boxes are emptied on the next
    /// render, the first point that has the `&mut Window` `set_value` needs.
    clarification_inputs_dirty: bool,
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
    /// Whether the inline plan card has scrolled above the viewport, resolved
    /// from the list's measured geometry in `prepare_render`.
    plan_above_viewport: bool,
    /// Whether the transcript list's scroll handler has been installed. Needs
    /// an entity handle, which `ChatView::new` does not have yet.
    scroll_handler_armed: bool,
    artifact_view: Entity<ArtifactView>,
    artifact_dismissed: bool,
    artifact_close_wired: bool,
    /// Tool id of the last query_data table auto-docked in the artifact panel.
    last_auto_opened_table_id: Option<String>,
    /// Tool id of the last create_chart PNG auto-docked in the artifact panel.
    last_auto_opened_chart_id: Option<String>,
    /// Tool id of the last Lane A browser call that opened the live
    /// screencast artifact (AGE-155) — guards against re-opening on every
    /// render pass while the same call is still in the transcript.
    last_auto_opened_browser_tool_id: Option<String>,
    /// Persists the right-pane width across dock open/close.
    artifact_split: Entity<ResizableState>,
    /// User expand/collapse for settled activity groups (`BlockId.0` → open).
    activity_expanded: HashMap<u64, bool>,
    /// Index into `messages` of the last assistant turn that is not streaming.
    /// Resolved once per frame; drives the action bar's always-visible state.
    last_settled_assistant_idx: Option<usize>,
    /// Turns for the frame being rendered, adapted once in `prepare_render`.
    ///
    /// The list renders one item at a time, so re-adapting every message per
    /// item would be quadratic. Shared rather than cloned: `Turn` owns its
    /// strings and block vectors.
    turns: Rc<Vec<Turn>>,
    /// Wall clock for the in-flight assistant turn (work-fold timer + duration stamp).
    stream_started_at: Option<Instant>,
    /// Hide the session "N files changed" bar after Keep all.
    session_review_dismissed: bool,
    /// Session files-changed bar is unfolded to the per-file list.
    session_bar_expanded: bool,
    elapsed_tick_started: bool,
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

/// Hash of everything about a turn that changes how tall it renders.
///
/// Lengths and flags, never contents: this runs for every turn every frame.
fn turn_fingerprint(turn: &Turn, activity_expanded: &HashMap<u64, bool>) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    turn.id.hash(&mut hasher);
    turn.collapsed.hash(&mut hasher);
    turn.streaming.hash(&mut hasher);
    turn.blocks.len().hash(&mut hasher);
    for block in &turn.blocks {
        std::mem::discriminant(block).hash(&mut hasher);
        block.id().0.hash(&mut hasher);
        match block {
            Block::User {
                content,
                attachments,
                ..
            } => {
                content.len().hash(&mut hasher);
                attachments.len().hash(&mut hasher);
            }
            Block::Text {
                content,
                attachments,
                ..
            } => {
                content.len().hash(&mut hasher);
                attachments.len().hash(&mut hasher);
            }
            Block::Thinking { block, .. } => block.summary.len().hash(&mut hasher),
            Block::Activity { id, tools } => {
                tools.len().hash(&mut hasher);
                activity_expanded.get(&id.0).hash(&mut hasher);
                // A failed run renders an extra line and forces the card open.
                tools
                    .iter()
                    .map(|tool| std::mem::discriminant(&tool.state))
                    .for_each(|state| state.hash(&mut hasher));
            }
            Block::ArtifactBatch { files, .. } => files.len().hash(&mut hasher),
            Block::Approval { approval, .. } => {
                std::mem::discriminant(&approval.state).hash(&mut hasher)
            }
            Block::Clarification { clarification, .. } => {
                // The summary gains an answer line per question once the user
                // replies, so the cached render must not outlive that.
                std::mem::discriminant(&clarification.state).hash(&mut hasher);
                clarification.answers.len().hash(&mut hasher);
            }
            Block::Error { message, .. } => message.len().hash(&mut hasher),
            Block::Diff { .. }
            | Block::Artifact { .. }
            | Block::TablePreview { .. }
            | Block::Plan { .. } => {}
        }
    }
    hasher.finish()
}

/// How far beyond the viewport the list renders and measures turns.
///
/// Roughly half a screen: enough that a flick-scroll does not show unmeasured
/// items popping into place, without measuring history nobody is looking at.
const TRANSCRIPT_OVERDRAW: Pixels = px(400.0);

/// Vertical separation between turns.
///
/// Padding, not margin: `List` measures each item with `layout_as_root`, and a
/// root element's margin is not part of the box it reports.
const TURN_GAP: Pixels = px(8.0);

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type a message...")
                .clean_on_escape()
                .auto_grow(2, 15)
        });

        // One free-text box per question slot the popover can show. Built here
        // because `InputState::new` needs a `Window`; the handler that receives
        // the questions off the stream has none.
        let clarification_inputs: Vec<Entity<InputState>> = (0..MAX_CLARIFYING_QUESTIONS)
            .map(|_| {
                cx.new(|cx| InputState::new(window, cx).placeholder("Or type your own answer…"))
            })
            .collect();

        let chat_input_state = cx.new(|_cx| ChatInputState::new(input.clone()));

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
        crate::boot_timing::checkpoint("to_composer_focusable");

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
            transcript_list: ListState::new(0, ListAlignment::Top, TRANSCRIPT_OVERDRAW),
            transcript_fingerprints: Vec::new(),
            last_font_size: GeneralSettingsModel::default().font_size,
            user_scrolled_away: false,
            collapsed_turns: HashMap::new(),
            pending_approval: None,
            pending_clarification: None,
            clarification_inputs,
            clarification_inputs_dirty: false,
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
            plan_above_viewport: false,
            scroll_handler_armed: false,
            artifact_view: new_artifact_view(window, cx),
            artifact_dismissed: false,
            artifact_close_wired: false,
            last_auto_opened_table_id: None,
            last_auto_opened_chart_id: None,
            last_auto_opened_browser_tool_id: None,
            artifact_split: cx.new(|_| ResizableState::default()),
            activity_expanded: HashMap::new(),
            last_settled_assistant_idx: None,
            turns: Rc::new(Vec::new()),
            stream_started_at: None,
            session_review_dismissed: false,
            session_bar_expanded: false,
            elapsed_tick_started: false,
        }
    }

    /// Get the chat input state entity (for wiring callbacks)
    pub fn chat_input_state(&self) -> &Entity<ChatInputState> {
        &self.chat_input_state
    }

    /// Get the artifact panel entity (for the top-right unfold button in
    /// `app_view.rs`/`titlebar.rs`, which needs to read its mode and toggle it).
    pub fn artifact_view(&self) -> &Entity<ArtifactView> {
        &self.artifact_view
    }

    /// Unfold/fold the artifact panel — mirrors the sidebar's own
    /// fold/unfold toggle. Reveals whatever was last open; folding it away
    /// again stays available from the panel's own close button and Escape.
    pub fn toggle_artifact_panel(&mut self, cx: &mut Context<Self>) {
        self.artifact_view.update(cx, |view, cx| {
            let next = if view.mode == ArtifactMode::Closed {
                ArtifactMode::Docked
            } else {
                ArtifactMode::Closed
            };
            view.set_mode(next, cx);
        });
    }

    /// Manually start a browser session in the artifact panel — independent
    /// of any agent tool call, from the unfold button's "Browser" menu item.
    /// Reuses the conversation's existing session if the agent already
    /// built one this turn, rather than starting a second, disconnected one.
    pub fn open_manual_browser(&mut self, cx: &mut Context<Self>) {
        let Some(conv_id) = self.conversation_id.clone() else {
            return;
        };
        let manager = chatty_core::services::browser::registry::for_conversation(&conv_id)
            .unwrap_or_else(|| {
                let settings = cx.try_global::<ExecutionSettingsModel>();
                let workspace = settings
                    .and_then(|s| s.workspace_dir.clone())
                    .map(std::path::PathBuf::from);
                let internet_access = settings.map(|s| s.fetch_enabled).unwrap_or(true);
                let manager = std::sync::Arc::new(if internet_access {
                    chatty_core::services::browser::BrowserManager::open_web(workspace)
                } else {
                    chatty_core::services::browser::BrowserManager::lane_a(workspace)
                });
                chatty_core::services::browser::registry::register(
                    conv_id.clone(),
                    manager.clone(),
                );
                manager
            });
        self.artifact_dismissed = false;
        self.artifact_view.update(cx, |view, cx| {
            view.open_browser(manager, cx);
        });
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
        if !matches!(msg.role, MessageRole::Assistant) {
            return false;
        }
        self.collapsed_turns
            .get(&index)
            .copied()
            .unwrap_or_else(|| {
                msg.is_streaming
                    || msg.system_trace_view.is_some()
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
        self.stream_started_at = Some(Instant::now());
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
        // Resolve the parent bubble *before* the sweep — the lookup keys off
        // `is_streaming`, which the sweep clears.
        let parent = self.parent_streaming_assistant_index();

        let Some(idx) = parent else {
            // No parent bubble, but rows may still be marked streaming. The
            // stream has ended either way, so clear them rather than leaving
            // the footer running (AGE-189).
            self.clear_all_streaming_flags();
            cx.notify();
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
            self.clear_all_streaming_flags();
            self.streaming_parse_cache = None;
            cx.notify();
            return;
        }

        self.stamp_trace_duration();
        self.clear_all_streaming_flags();
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

        if let Some(view) = self
            .messages
            .get(idx)
            .and_then(|m| m.system_trace_view.as_ref())
        {
            let trace = view.read(cx).get_trace().clone();
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if let Some(preview) = extract_table_preview(tool) {
                    self.try_auto_open_query_table(&tool.id, preview, cx);
                    break;
                }
                if let Some(spec) = extract_chart_spec(tool) {
                    self.try_auto_open_chart(&tool.id, spec, cx);
                    break;
                }
            }
        }

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
        // As in `finalize_assistant_message`: resolve the parent first, then
        // sweep, and sweep even when there is no parent to finalize.
        let Some(idx) = self.parent_streaming_assistant_index() else {
            self.clear_all_streaming_flags();
            cx.notify();
            return;
        };
        self.stamp_trace_duration();
        self.clear_all_streaming_flags();
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
        self.stamp_trace_duration();
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

    fn stamp_trace_duration(&mut self) {
        let Some(started) = self.stream_started_at else {
            return;
        };
        let elapsed = started.elapsed();
        for msg in self.messages.iter_mut().rev() {
            if let Some(trace) = msg.live_trace.as_mut() {
                if trace.total_duration.is_none() {
                    trace.total_duration = Some(elapsed);
                }
                return;
            }
        }
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
        self.scroll_transcript_to_bottom();
    }

    /// Scroll the transcript to the true bottom of its content.
    ///
    /// Anchoring past the last item rather than at a pixel offset is what
    /// makes this correct: `ListState` backfills from the content end using
    /// the heights it measured, so a last turn taller than the viewport still
    /// reaches the bottom (AGE-180), and no arithmetic here can disagree with
    /// what was painted.
    fn scroll_transcript_to_bottom(&mut self) {
        let item_ix = self.transcript_list.item_count();
        self.transcript_list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(0.0),
        });
    }

    /// If sticky-scroll is active, re-assert the bottom for this frame.
    /// Used for incremental streaming updates — respects the user's decision
    /// to scroll up by not re-enabling sticky mode.
    fn scroll_if_sticky(&mut self) {
        if self.stick_to_bottom {
            self.scroll_transcript_to_bottom();
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

    /// Whether to show the animated "thinking" indicator at the bottom of the
    /// message list.
    ///
    /// The authority is `StreamManager`, not the per-message `is_streaming`
    /// flags: those are cleared one message at a time, so a row the teardown
    /// path missed (a sub-agent progress row whose finalize never arrived) kept
    /// the footer counting up forever with no turn header above it — AGE-188
    /// and AGE-189. Message flags still drive per-bubble rendering; they just
    /// no longer decide whether the agent is working.
    ///
    /// Falls back to the message scan when there is no StreamManager (tests,
    /// early startup).
    fn is_thinking_indicator_visible(&self, cx: &App) -> bool {
        let manager = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|global| global.get());
        match (manager, self.conversation_id.as_ref()) {
            (Some(manager), Some(conv_id)) => manager.read(cx).is_streaming(conv_id),
            _ => self.any_assistant_message_streaming(),
        }
    }

    fn any_assistant_message_streaming(&self) -> bool {
        self.messages
            .iter()
            .any(|msg| matches!(msg.role, MessageRole::Assistant) && msg.is_streaming)
    }

    /// Clear `is_streaming` on every assistant message.
    ///
    /// `finalize_assistant_message` and `mark_message_cancelled` each clear a
    /// single message — the parent bubble — so any other row left streaming
    /// outlived its stream. Nothing is streaming once the stream has ended, so
    /// the sweep is unconditional (AGE-189).
    fn clear_all_streaming_flags(&mut self) {
        let cleared = parent_stream::clear_streaming_flags(&mut self.messages);
        if cleared > 1 {
            trace!(
                target: "chatty_gpui::render::stream",
                cleared,
                "Cleared streaming flags left behind by an earlier row"
            );
        }
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

    fn ensure_artifact_close_wired(&mut self, cx: &mut Context<Self>) {
        if self.artifact_close_wired {
            return;
        }
        self.artifact_close_wired = true;
        cx.subscribe(
            &self.artifact_view,
            |this, _, event: &ArtifactViewEvent, cx| {
                if matches!(event, ArtifactViewEvent::Closed) {
                    this.artifact_dismissed = true;
                }
                cx.notify();
            },
        )
        .detach();
    }

    /// Latest PDF produced by a tool (Typst / pdf_* / write…pdf). Used to auto-dock
    /// the artifact panel — non-PDF produced files stay click-to-open only.
    fn last_pdf_artifact(&self, cx: &App) -> Option<(PathBuf, String)> {
        let traces = self.history_traces(cx);
        for (msg, hist) in self.messages.iter().zip(traces.iter()).rev() {
            let Some(trace) = msg.live_trace.as_ref().or(hist.as_ref()) else {
                continue;
            };
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if !matches!(
                    tool.state,
                    chatty_core::models::message_types::ToolCallState::Success
                ) || !is_pdf_artifact_tool(&tool.tool_name, &tool.input, tool.output.as_deref())
                {
                    continue;
                }
                let Some(path) = tool
                    .output
                    .as_deref()
                    .and_then(tool_file_path)
                    .or_else(|| tool_file_path(&tool.input))
                else {
                    continue;
                };
                if !is_pdf_path(&path) || !produced_path_is_openable(&path) {
                    continue;
                }
                return Some((path.clone(), read_artifact_source(&path)));
            }
        }
        None
    }

    fn maybe_open_pdf_artifact(&mut self, cx: &mut Context<Self>) {
        self.ensure_artifact_close_wired(cx);
        if self.artifact_dismissed {
            return;
        }
        if self.artifact_view.read(cx).mode != ArtifactMode::Closed {
            return;
        }
        let Some((path, source)) = self.last_pdf_artifact(cx) else {
            return;
        };
        self.show_artifact(path, source, None, cx);
    }

    fn last_chart_spec(
        &self,
        cx: &App,
    ) -> Option<(String, chatty_core::tools::chart_tool::ChartSpec)> {
        let traces = self.history_traces(cx);
        for (msg, hist) in self.messages.iter().zip(traces.iter()).rev() {
            if msg.is_streaming {
                continue;
            }
            let Some(trace) = msg.live_trace.as_ref().or(hist.as_ref()) else {
                continue;
            };
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if let Some(spec) = extract_chart_spec(tool) {
                    return Some((tool.id.clone(), spec));
                }
            }
        }
        None
    }

    fn maybe_open_chart_artifact(&mut self, cx: &mut Context<Self>) {
        self.ensure_artifact_close_wired(cx);
        if let Some((tool_id, spec)) = self.last_chart_spec(cx) {
            self.try_auto_open_chart(&tool_id, spec, cx);
            return;
        }
        let Some((tool_id, path)) = self.last_attachment_image(cx) else {
            return;
        };
        self.try_auto_open_image_artifact(&tool_id, path, cx);
    }

    /// Latest Lane A browser tool call, streaming or not — unlike the chart
    /// and PDF triggers, this must fire while the call is still `Running`:
    /// the whole point of the screencast (AGE-155) is watching the agent
    /// drive the page live, not waiting for the turn to finish.
    fn last_browser_tool_call(&self, cx: &App) -> Option<String> {
        let traces = self.history_traces(cx);
        for (msg, hist) in self.messages.iter().zip(traces.iter()).rev() {
            let Some(trace) = msg.live_trace.as_ref().or(hist.as_ref()) else {
                continue;
            };
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if is_lane_a_browser_tool(&tool.tool_name) {
                    return Some(tool.id.clone());
                }
            }
        }
        None
    }

    fn maybe_open_browser_artifact(&mut self, cx: &mut Context<Self>) {
        self.ensure_artifact_close_wired(cx);
        // Deliberately not gated on `artifact_dismissed`, unlike the other
        // artifact kinds: this is a live session the human-takeover feature
        // (AGE-156) depends on being reachable, not a one-shot file preview.
        // Closing the panel hides it for now; the next browser tool call
        // (a fresh `tool_id`, deduped below) brings it back rather than
        // leaving the take-control lock permanently unreachable.
        let Some(tool_id) = self.last_browser_tool_call(cx) else {
            return;
        };
        if self.last_auto_opened_browser_tool_id.as_deref() == Some(tool_id.as_str()) {
            return;
        }
        let Some(conversation_id) = self.conversation_id.clone() else {
            return;
        };
        let Some(manager) =
            chatty_core::services::browser::registry::for_conversation(&conversation_id)
        else {
            return;
        };
        self.last_auto_opened_browser_tool_id = Some(tool_id);
        self.artifact_dismissed = false;
        self.artifact_view.update(cx, |view, cx| {
            view.open_browser(manager, cx);
        });
        cx.notify();
    }

    fn last_attachment_image(&self, cx: &App) -> Option<(String, PathBuf)> {
        let traces = self.history_traces(cx);
        for (msg, hist) in self.messages.iter().zip(traces.iter()).rev() {
            if msg.is_streaming {
                continue;
            }
            let Some(trace) = msg.live_trace.as_ref().or(hist.as_ref()) else {
                continue;
            };
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if let Some(path) = attachment_image_path(tool) {
                    return Some((tool.id.clone(), path));
                }
            }
        }
        None
    }

    fn try_auto_open_chart(
        &mut self,
        tool_id: &str,
        spec: chatty_core::tools::chart_tool::ChartSpec,
        cx: &mut Context<Self>,
    ) {
        if self.last_auto_opened_chart_id.as_deref() == Some(tool_id) {
            return;
        }
        self.last_auto_opened_chart_id = Some(tool_id.to_string());
        self.show_chart(spec, cx);
    }

    fn try_auto_open_image_artifact(
        &mut self,
        tool_id: &str,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.last_auto_opened_chart_id.as_deref() == Some(tool_id) {
            return;
        }
        self.last_auto_opened_chart_id = Some(tool_id.to_string());
        self.show_artifact(path, String::new(), None, cx);
    }

    fn show_chart(
        &mut self,
        spec: chatty_core::tools::chart_tool::ChartSpec,
        cx: &mut Context<Self>,
    ) {
        self.ensure_artifact_close_wired(cx);
        self.artifact_dismissed = false;
        self.artifact_view.update(cx, |view, cx| {
            view.open_chart(spec, cx);
        });
        cx.notify();
    }

    fn last_query_table_preview(
        &self,
        cx: &App,
    ) -> Option<(String, chatty_core::tools::data_query_tool::TablePreview)> {
        let traces = self.history_traces(cx);
        for (msg, hist) in self.messages.iter().zip(traces.iter()).rev() {
            if msg.is_streaming {
                continue;
            }
            let Some(trace) = msg.live_trace.as_ref().or(hist.as_ref()) else {
                continue;
            };
            for item in trace.items.iter().rev() {
                let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item else {
                    continue;
                };
                if tool.tool_name != "query_data" {
                    continue;
                }
                if !matches!(
                    tool.state,
                    chatty_core::models::message_types::ToolCallState::Success
                ) {
                    continue;
                }
                if let Some(preview) = extract_table_preview(tool) {
                    return Some((tool.id.clone(), preview));
                }
            }
        }
        None
    }

    fn maybe_open_query_table(&mut self, cx: &mut Context<Self>) {
        self.ensure_artifact_close_wired(cx);
        let Some((tool_id, preview)) = self.last_query_table_preview(cx) else {
            return;
        };
        self.try_auto_open_query_table(&tool_id, preview, cx);
    }

    fn try_auto_open_query_table(
        &mut self,
        tool_id: &str,
        preview: chatty_core::tools::data_query_tool::TablePreview,
        cx: &mut Context<Self>,
    ) {
        if self.last_auto_opened_table_id.as_deref() == Some(tool_id) {
            return;
        }
        self.last_auto_opened_table_id = Some(tool_id.to_string());
        self.show_table(preview, cx);
    }

    fn show_artifact(
        &mut self,
        path: PathBuf,
        source: String,
        old: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.ensure_artifact_close_wired(cx);
        self.artifact_dismissed = false;
        let workspace = cx
            .try_global::<ExecutionSettingsModel>()
            .and_then(|s| s.workspace_dir.clone())
            .map(PathBuf::from);
        let resolved = super::transcript::resolve_artifact_path(&path, workspace.as_deref());
        // `resolve_artifact_path` hands back the original path when it finds
        // the file nowhere, and the PDF viewer then asks pdfium to open it
        // relative to the process CWD — which is how a compile that never wrote
        // its file surfaced as "No such file or directory" in the panel.
        if !resolved.exists() {
            warn!(
                path = %path.display(),
                resolved = %resolved.display(),
                "Refusing to open an artifact that is not on disk"
            );
            return;
        }
        // Relative tool paths often yield an empty source; re-read after resolve.
        let source = {
            let from_disk = super::transcript::read_artifact_source(&resolved);
            if from_disk.is_empty() {
                source
            } else {
                from_disk
            }
        };
        self.artifact_view.update(cx, |view, cx| {
            view.open(
                resolved,
                source,
                old,
                cx.try_global::<ExecutionSettingsModel>()
                    .and_then(|s| s.workspace_dir.clone()),
                cx,
            );
        });
        cx.notify();
    }

    fn show_table(
        &mut self,
        preview: chatty_core::tools::data_query_tool::TablePreview,
        cx: &mut Context<Self>,
    ) {
        self.ensure_artifact_close_wired(cx);
        self.artifact_dismissed = false;
        self.artifact_view.update(cx, |view, cx| {
            view.open_table(preview, cx);
        });
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

    fn session_file_changes(&self, cx: &App) -> Vec<FileChange> {
        let mut collected = Vec::new();
        for msg in &self.messages {
            let live = msg.live_trace.as_ref();
            let stored = msg
                .system_trace_view
                .as_ref()
                .map(|view| view.read(cx).get_trace());
            let trace = live.or(stored);
            if let Some(trace) = trace {
                for item in &trace.items {
                    if let crate::chatty::views::message_types::TraceItem::ToolCall(tool) = item
                        && let Some(change) = file_change_from_tool(tool)
                    {
                        collected.push(change);
                    }
                }
            }
        }
        merge_file_changes(collected)
    }

    fn open_session_review(&mut self, focus: Option<PathBuf>, cx: &mut Context<Self>) {
        let workspace = cx
            .try_global::<ExecutionSettingsModel>()
            .and_then(|s| s.workspace_dir.clone());
        let workspace_path = workspace.as_ref().map(PathBuf::from);
        let focus_resolved = focus.map(|p| resolve_artifact_path(&p, workspace_path.as_deref()));
        let changes = self.session_file_changes(cx);
        let files: Vec<_> = changes
            .into_iter()
            .map(|change| {
                let resolved = resolve_artifact_path(&change.path, workspace_path.as_deref());
                let new = if change.new.is_empty() {
                    read_artifact_source(&resolved)
                } else {
                    change.new
                };
                let old = if change.old.is_empty() {
                    None
                } else {
                    Some(change.old)
                };
                (resolved, new, old)
            })
            .collect();
        if files.is_empty() {
            return;
        }
        self.ensure_artifact_close_wired(cx);
        self.artifact_dismissed = false;
        self.artifact_view.update(cx, |view, cx| {
            view.open_review(files, workspace, focus_resolved, cx);
        });
        cx.notify();
    }

    fn ensure_elapsed_tick(&mut self, cx: &mut Context<Self>) {
        if self.elapsed_tick_started || !self.is_thinking_indicator_visible(cx) {
            return;
        }
        self.elapsed_tick_started = true;
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = entity
                    .update(cx, |this, cx| {
                        if this.is_thinking_indicator_visible(cx) {
                            cx.notify();
                            true
                        } else {
                            this.elapsed_tick_started = false;
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    /// Pre-render side effects: sticky scroll, input clearing, model refresh.
    fn prepare_render(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_clarification_inputs(window, cx);
        self.ensure_scroll_handler(cx);
        self.turns = Rc::new(self.typed_turns(cx));
        self.last_settled_assistant_idx = self
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.role, TurnRole::Assistant) && !turn.streaming)
            .map(|turn| turn.message_index);

        // Order matters for the rest of this function:
        //   1. sync the list so its item count matches `turns`,
        //   2. read the plan card's position from the geometry the last frame
        //      measured — before any scrolling moves the anchor,
        //   3. resolve the scroll policy and re-assert the bottom.
        // Reading plan geometry after step 3 would see the anchor parked past
        // the last item and report every turn as "above the viewport".
        let font_size = cx
            .try_global::<GeneralSettingsModel>()
            .map(|settings| settings.font_size)
            .unwrap_or_else(|| GeneralSettingsModel::default().font_size);
        self.sync_transcript_list(font_size);
        self.plan_above_viewport = self.compute_plan_above_viewport();

        let running = self.is_thinking_indicator_visible(cx);
        let pending_approval = self.pending_approval.is_some();
        self.artifact_view.update(cx, |view, cx| {
            view.set_chrome(running, pending_approval, cx);
        });

        // Sticky-scroll and pin visibility are both derived from how far the
        // transcript currently is from its bottom, rather than latched on the
        // first frame that drifts (AGE-180).
        let offset = self.transcript_list.scroll_px_offset_for_scrollbar();
        let max_offset = self.transcript_list.max_offset_for_scrollbar();
        let distance_from_bottom = max_offset.height + offset.y;
        let measured = max_offset.height > px(0.0);

        let decision = scroll::resolve_scroll_state(
            distance_from_bottom,
            measured,
            self.stick_to_bottom,
            self.user_scrolled_away,
        );
        if decision.stick != self.stick_to_bottom || decision.show_pin != self.user_scrolled_away {
            trace!(
                distance = %distance_from_bottom,
                stick = decision.stick,
                show_pin = decision.show_pin,
                "Transcript scroll state changed"
            );
        }
        self.stick_to_bottom = decision.stick;
        self.user_scrolled_away = decision.show_pin;
        if decision.stick {
            self.scroll_transcript_to_bottom();
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

        // Refresh available models from global store (safety net if a change
        // notification was missed). Sync even when the list is empty so deletes
        // clear the picker.
        if let Some(models_model) = cx.try_global::<ModelsModel>() {
            let models_list: Vec<ModelOption> = models_model
                .models()
                .iter()
                .map(|m| ModelOption::new(m.id.clone(), m.name.clone(), m.provider_type.clone()))
                .collect();

            self.chat_input_state.update(cx, |state, cx| {
                if state.available_models() != models_list.as_slice() {
                    let default_model_id = models_list.first().map(|model| model.id.clone());
                    state.set_available_models(models_list, default_model_id, cx);
                }
            });
        }
    }

    /// Keep the list's item count and cached heights honest about `self.turns`.
    ///
    /// `List` re-renders and re-measures whatever is on screen every frame, so
    /// this exists for the turns that are *not*: it tells the list which of
    /// them changed shape, so the scroll range and `bounds_for_item` stay true.
    ///
    /// The fingerprint deliberately hashes lengths rather than contents —
    /// O(blocks), not O(bytes). Streaming text is append-only, so its length
    /// always moves; a change that preserved every length would go unnoticed,
    /// and would cost only a slightly stale scroll range for an off-screen
    /// turn, never an overlap.
    fn sync_transcript_list(&mut self, font_size: f32) {
        let next: Vec<u64> = self
            .turns
            .iter()
            .map(|turn| turn_fingerprint(turn, &self.activity_expanded))
            .collect();
        let previous = std::mem::replace(&mut self.transcript_fingerprints, next);
        let next = &self.transcript_fingerprints;

        // A font change re-flows every turn, and `List` only drops its cached
        // heights when the list's width changes.
        if font_size != self.last_font_size || self.transcript_list.item_count() != previous.len() {
            self.last_font_size = font_size;
            self.transcript_list.reset(next.len());
            return;
        }

        let common = previous
            .iter()
            .zip(next.iter())
            .take_while(|(a, b)| a == b)
            .count();
        if previous.len() == next.len() {
            for ix in common..next.len() {
                if previous[ix] != next[ix] {
                    self.transcript_list.splice(ix..ix + 1, 1);
                }
            }
        } else {
            // Appends splice past the anchor, so the user's scroll position
            // survives a new turn arriving.
            self.transcript_list
                .splice(common..previous.len(), next.len() - common);
        }
    }

    /// Stop following the bottom as soon as the user scrolls the transcript.
    ///
    /// The distance-based policy in `prepare_render` cannot do this on its own:
    /// a trackpad delivers a few pixels per event, and each event re-renders,
    /// so a slow scroll would be pulled back to the bottom before it ever
    /// accumulated past [`scroll::STICKY_BOTTOM_EPSILON`]. This ends following
    /// on the first event instead; `prepare_render` restores it when the user
    /// scrolls back down, since scrolling at the bottom clamps and leaves the
    /// distance at zero.
    ///
    /// Scrollbar drags do not come through here (`set_offset_from_scrollbar`
    /// never calls the handler) — they move far enough in one event for the
    /// distance check to handle them.
    ///
    /// The handler runs while `ListState`'s `RefCell` is mutably borrowed, so
    /// it must only touch `ChatView`: calling back into `self.transcript_list`
    /// from in here would panic.
    fn ensure_scroll_handler(&mut self, cx: &mut Context<Self>) {
        if self.scroll_handler_armed {
            return;
        }
        self.scroll_handler_armed = true;

        let view = cx.entity().downgrade();
        self.transcript_list
            .set_scroll_handler(move |_event, _window, cx| {
                view.update(cx, |view, cx| {
                    if view.stick_to_bottom {
                        view.stick_to_bottom = false;
                        cx.notify();
                    }
                })
                .ok();
            });
    }

    /// Drop every measured height and anchor. For a conversation switch, where
    /// nothing about the old transcript's geometry applies to the new one.
    pub(super) fn reset_transcript_list(&mut self) {
        self.transcript_list.reset(0);
        self.transcript_fingerprints.clear();
    }

    /// Whether the inline plan card has scrolled above the viewport.
    fn compute_plan_above_viewport(&self) -> bool {
        let Some(ix) = plan_turn_index(&self.turns) else {
            return false;
        };
        scroll::plan_is_above_viewport(
            self.transcript_list.bounds_for_item(ix),
            ix,
            self.transcript_list.logical_scroll_top().item_ix,
            self.transcript_list.viewport_bounds().top(),
        )
    }

    /// Render the scrollable message list area including the loading skeleton.
    fn render_message_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_awaiting = self.is_awaiting_response();
        // Adapted once per frame in `prepare_render`.
        let turns = self.turns.clone();
        let show_start_screen = turns.is_empty() && !is_awaiting;
        let thinking_visible = self.is_thinking_indicator_visible(cx);
        if thinking_visible {
            self.ensure_elapsed_tick(cx);
            // Only name a tool that is still running. Taking the last tool
            // call regardless of state left the footer reading "Conjuring
            // browser_navigate" long after that call had finished or errored
            // out (AGE-188).
            let attention = self.messages.iter().rev().find_map(|msg| {
                msg.live_trace.as_ref().and_then(|trace| {
                    trace.items.iter().rev().find_map(|item| match item {
                        crate::chatty::views::message_types::TraceItem::ToolCall(tool)
                            if matches!(
                                tool.state,
                                chatty_core::models::message_types::ToolCallState::Running
                            ) =>
                        {
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
                // Clear the name when no tool is running, rather than leaving
                // the last one in place (AGE-188).
                indicator.set_attention(attention.unwrap_or_default(), cx);
                indicator.set_progress(steps_done, steps_total, cx);
            });
        }
        let thinking_indicator = self.thinking_indicator.clone();
        let has_plan = self.plan_snapshot_active();
        let entity = cx.entity();
        let session_entity = entity.clone();
        let session_bar_expanded = self.session_bar_expanded;
        let session_changes = if self.session_review_dismissed {
            Vec::new()
        } else {
            self.session_file_changes(cx)
        };
        let user_away = self.user_scrolled_away;
        let has_approval = self.active_approval_for_display().is_some();
        // Resolved from the list's measured geometry in `prepare_render`.
        let show_strip = has_plan && self.plan_above_viewport;
        if !show_strip {
            self.plan_overlay_open = false;
        }
        let plan_strip = self.agent_task_snapshot.clone().filter(|_| show_strip);
        let jump_turn = plan_turn_index(&turns);
        let jump_message = jump_turn.and_then(|ix| turns.get(ix).map(|turn| turn.message_index));
        let overlay_open = self.plan_overlay_open;
        let pending_command = self
            .active_approval_for_display()
            .map(|p| p.command.clone());

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
            .overflow_hidden()
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
                                .pending_command(pending_command)
                                .on_open_change({
                                    let entity = entity.clone();
                                    move |open, cx| {
                                        entity.update(cx, |view, cx| {
                                            view.plan_overlay_open = open;
                                            cx.notify();
                                        });
                                    }
                                })
                                .on_decide({
                                    let entity = entity.clone();
                                    move |approved, cx| {
                                        entity.update(cx, |view, cx| {
                                            view.handle_floating_approval(approved, cx);
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
                                            // Without this, the next frame's
                                            // sticky re-assert yanks the view
                                            // straight back to the bottom.
                                            view.stick_to_bottom = false;
                                            if let Some(turn_ix) = jump_turn {
                                                view.transcript_list.scroll_to_reveal_item(turn_ix);
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
                        .overflow_hidden()
                        .on_scroll_wheel({
                            let entity = entity.clone();
                            move |_, _, cx| {
                                entity.update(cx, |_, cx| cx.notify());
                            }
                        })
                        .child(
                            // Horizontal padding and the gap between turns live
                            // on the item, not here: `List` places items at the
                            // list's left edge and measures them at full width,
                            // and an item's own margin is not part of the box it
                            // reports.
                            list(self.transcript_list.clone(), {
                                let entity = entity.clone();
                                move |ix, window, cx| {
                                    entity.update(cx, |view, cx| view.render_turn(ix, window, cx))
                                }
                            })
                            .pt_4()
                            .pb_12()
                            .size_full(),
                        )
                        .when(overlay_open, |this| {
                            this.child(
                                div()
                                    .id("plan-overlay-scrim")
                                    .absolute()
                                    .inset_0()
                                    .bg(cx.theme().overlay),
                            )
                        })
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
                        ),
                )
                .vertical_scrollbar(&self.transcript_list)
            })
            .when(!session_changes.is_empty(), |this| {
                this.child(
                    SessionChangeBar::new(session_changes)
                        .expanded(session_bar_expanded)
                        .on_toggle({
                            let entity = session_entity.clone();
                            move |cx| {
                                entity.update(cx, |view, cx| {
                                    view.session_bar_expanded = !view.session_bar_expanded;
                                    cx.notify();
                                });
                            }
                        })
                        .on_review({
                            let entity = session_entity.clone();
                            move |cx| {
                                entity.update(cx, |view, cx| {
                                    view.open_session_review(None, cx);
                                });
                            }
                        })
                        .on_open_file({
                            let entity = session_entity.clone();
                            move |path, cx| {
                                entity.update(cx, |view, cx| {
                                    view.open_session_review(Some(path), cx);
                                });
                            }
                        })
                        .on_keep_all({
                            let entity = session_entity;
                            move |cx| {
                                entity.update(cx, |view, cx| {
                                    view.session_review_dismissed = true;
                                    view.session_bar_expanded = false;
                                    cx.notify();
                                });
                            }
                        }),
                )
            })
            .when(thinking_visible, |this| this.child(thinking_indicator))
    }

    /// Render one transcript turn.
    ///
    /// One item at a time, because the measuring list asks for items
    /// individually. Everything shared across a frame — the adapted turns, the
    /// index of the last settled assistant turn — is resolved in
    /// `prepare_render`, not recomputed here.
    fn render_turn(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(turn) = self.turns.get(ix).cloned() else {
            return div().into_any_element();
        };
        let inner = self.render_turn_body(turn, window, cx);
        div()
            .w_full()
            .px_4()
            .pb(TURN_GAP)
            .child(inner)
            .into_any_element()
    }

    /// The turn itself, without the list's own insets.
    fn render_turn_body(
        &mut self,
        turn: Turn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let last_visible_assistant_idx = self.last_settled_assistant_idx;

        let entity = cx.entity();
        let plan = self.agent_task_snapshot.clone();
        let open_artifact = self.artifact_view.read(cx).path().cloned();
        let live_elapsed = self.stream_started_at.map(|t| t.elapsed()).or_else(|| {
            self.is_thinking_indicator_visible(cx)
                .then(|| self.thinking_indicator.read(cx).elapsed())
        });
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
            Rc::new(move |open: ArtifactOpen, cx| {
                entity.update(cx, |view, cx| {
                    view.show_artifact(open.path, open.source, open.old, cx);
                });
            })
        };
        let on_open_table: OpenTable = {
            let entity = entity.clone();
            Rc::new(move |open: TableOpen, cx| {
                entity.update(cx, |view, cx| {
                    view.show_table(open.preview, cx);
                });
            })
        };
        let on_activity_toggle = {
            let entity = entity.clone();
            Rc::new(move |block_id: u64, cx: &mut App| {
                entity.update(cx, |view, cx| {
                    let current = view.activity_expanded.get(&block_id).copied();
                    // Missing key → settled success is collapsed; toggle opens.
                    let next = !current.unwrap_or(false);
                    view.activity_expanded.insert(block_id, next);
                    cx.notify();
                });
            })
        };

        // Folded turns keep receipts + the assistant message; only the
        // work trace (thinking / activity / diffs) is hidden.
        let typed: Vec<AnyElement> = turn
            .blocks
            .iter()
            .filter(|block| block_visible_in_turn(&turn, block))
            .map(|block| {
                let activity_open = match block {
                    Block::Activity { id, .. } => self.activity_expanded.get(&id.0).copied(),
                    _ => None,
                };
                render_typed_block(
                    block,
                    turn.message_index,
                    Some(on_open.clone()),
                    Some(on_open_table.clone()),
                    plan.as_ref(),
                    activity_open,
                    Some(on_activity_toggle.clone()),
                    open_artifact.as_deref(),
                    window,
                    cx,
                )
            })
            .collect();

        let show_work_fold = turn_has_work_fold(&turn);
        let work_header = show_work_fold.then(|| {
            let streaming = turn.streaming;
            let label = if streaming {
                format_working_for(live_elapsed.unwrap_or_default())
            } else {
                format_worked_for(turn.elapsed)
            };
            let msg_index = turn.message_index;
            let entity = entity.clone();
            let collapsed = turn.collapsed;
            let chevron = if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            };
            div()
                .id(ElementId::NamedInteger("turn-fold".into(), turn.id))
                .h(px(super::transcript::COLLAPSED_TURN_HEIGHT))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .when(streaming, |this| this.text_sm())
                .when(!streaming, |this| this.text_xs())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if streaming {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    entity.update(cx, |view, cx| {
                        view.collapsed_turns.insert(msg_index, !collapsed);
                        cx.notify();
                    });
                })
                .child(label)
                .child(
                    Icon::new(chevron)
                        .size_3()
                        .text_color(cx.theme().muted_foreground),
                )
                .into_any_element()
        });
        let file_overview = (!turn.collapsed && show_work_fold).then(|| {
            let changes = file_changes_from_turn(&turn);
            (!changes.is_empty()).then(|| TurnFileOverview::new(changes).into_any_element())
        });
        let skip_empty_message = msg.content.is_empty() && work_header.is_some();

        let mut msg_for_text = msg.clone();
        if !typed.is_empty() || work_header.is_some() {
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
                    let current = chat_view.diff_expanded.get(&key).copied().unwrap_or(false);
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
            Some(on_open_table.clone()),
            cx,
        );
        if streaming_slot.is_some() {
            self.streaming_parse_cache = streaming_slot;
        }
        if typed.is_empty() && work_header.is_none() {
            text
        } else {
            let gap = if skip_empty_message && typed.is_empty() {
                px(2.)
            } else {
                px(8.)
            };
            div()
                .flex()
                .flex_col()
                .gap(gap)
                .w_full()
                .children(work_header)
                .children(file_overview.flatten())
                .children(typed)
                .when(!skip_empty_message, |this| this.child(text))
                .into_any_element()
        }
    }

    /// Empty the free-text boxes so answers from an earlier request cannot leak
    /// into this one. Split out of the stream-event handler because
    /// `InputState::set_value` needs `&mut Window`, which that handler lacks.
    fn reset_clarification_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.clarification_inputs_dirty {
            return;
        }
        for slot in &self.clarification_inputs {
            slot.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        self.clarification_inputs_dirty = false;
    }

    /// Return the pending clarification if it belongs to the current conversation.
    fn active_clarification_for_display(&self) -> Option<PendingClarificationDisplay> {
        self.pending_clarification
            .as_ref()
            .filter(|pending| self.conversation_id.as_ref() == Some(&pending.conversation_id))
            .map(|pending| PendingClarificationDisplay {
                id: pending.id.clone(),
                questions: pending.questions.clone(),
                choices: pending.choices.clone(),
            })
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
                !msg.is_streaming
                    || !msg.content.is_empty()
                    || matches!(msg.role, MessageRole::Assistant)
                    || msg
                        .live_trace
                        .as_ref()
                        .is_some_and(|trace| trace.has_items())
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
        self.maybe_open_pdf_artifact(cx);
        self.maybe_open_query_table(cx);
        self.maybe_open_chart_artifact(cx);
        self.maybe_open_browser_artifact(cx);
        let docked = self.artifact_view.read(cx).mode == ArtifactMode::Docked;
        let full = self.artifact_view.read(cx).mode == ArtifactMode::Full;
        let artifact = self.artifact_view.clone();
        let split = self.artifact_split.clone();

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
            .when_some(self.active_clarification_for_display(), |this, pending| {
                let view_entity = cx.entity();
                let can_submit = self.clarification_ready(cx);
                this.child(
                    div().px_4().child(
                        ClarificationCard::new(
                            pending.id,
                            pending.questions,
                            pending.choices,
                            self.clarification_inputs.clone(),
                            can_submit,
                        )
                        .on_choose({
                            let entity = view_entity.clone();
                            move |question_id, option_ix, cx| {
                                entity.update(cx, |view, cx| {
                                    view.choose_clarification_option(question_id, option_ix, cx);
                                });
                            }
                        })
                        .on_submit({
                            let entity = view_entity.clone();
                            move |cx| {
                                entity.update(cx, |view, cx| {
                                    view.submit_clarification(cx);
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

        let root = div()
            .flex_1()
            .h_full()
            .w_full()
            .flex()
            .flex_row()
            .bg(cx.theme().background)
            .overflow_hidden();

        if full {
            root.child(div().flex_1().size_full().min_w_0().child(artifact))
                .into_any_element()
        } else if docked {
            root.child(
                div().flex_1().size_full().min_w_0().child(
                    h_resizable("chat-artifact-split")
                        .with_state(&split)
                        .child(resizable_panel().child(column))
                        .child(
                            resizable_panel()
                                .size(px(380.))
                                .size_range(px(280.)..px(900.))
                                .child(artifact.into_any_element()),
                        ),
                ),
            )
            .into_any_element()
        } else {
            root.child(column).into_any_element()
        }
    }
}
