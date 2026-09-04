//! Stream-event handlers for `ChatView`.
//!
//! # What lives here
//!
//! All `impl ChatView` methods that mutate the message list **in response
//! to events arriving from `StreamManager`** (tool calls, approvals,
//! thinking blocks) plus the keyboard-driven approval helpers
//! (`handle_floating_approval`, `expand_trace_to_approval`).
//!
//! These are split out because they share a common shape — locate the
//! active streaming message, mutate its `live_trace`, push the change to
//! the `SystemTraceView` entity — and grouping them makes it easier to
//! reason about the streaming-state machine without scrolling through
//! the entire view.
//!
//! # What does NOT live here
//!
//! - Streaming text deltas — see `append_assistant_text` in `mod.rs`.
//! - History loading / sub-agent progress — see `history.rs` and
//!   `sub_agent.rs`.
//! - The `Render` path — `mod.rs`.

use gpui::*;
use std::time::SystemTime;
use tracing::{debug, trace, warn};

use super::super::message_types::{
    ApprovalBlock, ApprovalState, ClarificationBlock, ClarificationState, ThinkingBlock,
    ThinkingState, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
    classify_initial_execution_engine, detect_execution_engine, friendly_tool_name,
    is_denial_result, predict_execution_engine,
};
use super::super::trace_components::SystemTraceView;
use super::{ChatView, PendingApprovalInfo, PendingClarificationInfo};
use crate::chatty::views::chart_renderer::extract_chart_spec;
use crate::chatty::views::transcript::ChosenOption;
use crate::chatty::views::transcript::{attachment_image_path, extract_table_preview};
use chatty_core::models::clarification_store::{ClarificationAnswer, ClarifyingQuestion};
use std::collections::HashMap;

impl ChatView {
    /// Handle tool call started event
    pub fn handle_tool_call_started(
        &mut self,
        id: String,
        name: String,
        source: ToolSource,
        cx: &mut Context<Self>,
    ) {
        debug!(tool_id = %id, tool_name = %name, "UI: handle_tool_call_started called");

        let Some(parent_idx) = self.parent_streaming_assistant_index().or_else(|| {
            if self.sub_agent_progress_msg_idx.is_some() {
                self.start_assistant_message(cx);
                Some(self.messages.len() - 1)
            } else {
                None
            }
        }) else {
            debug!("No parent streaming assistant for tool call");
            cx.notify();
            return;
        };

        // Capture current message content as "text_before" for interleaved rendering
        let text_before = self.messages[parent_idx].content.clone();

        let had_trace_view = self.messages[parent_idx].system_trace_view.is_some();
        let live_trace_items = self.messages[parent_idx]
            .live_trace
            .as_ref()
            .map(|t| t.items.len())
            .unwrap_or(0);

        tracing::trace!(
            target: "chatty_gpui::render::handler",
            event = "tool_call_started",
            tool_id = %id,
            tool_name = %name,
            text_before_len = text_before.len(),
            had_trace_view,
            live_trace_items,
            "tool_call_started",
        );

        debug!(
            tool_id = %id,
            tool_name = %name,
            text_before_len = text_before.len(),
            text_before_preview = %text_before.chars().take(50).collect::<String>(),
            "Captured text_before for tool call"
        );

        let display_name = friendly_tool_name(&name);
        let execution_engine = classify_initial_execution_engine(&name);
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
            source,
            execution_engine,
        };

        // Update live trace and create/update system_trace_view entity
        let mut new_tool_idx: Option<usize> = None;
        let last = &mut self.messages[parent_idx];
        debug!(
            has_last_message = true,
            is_streaming = last.is_streaming,
            has_live_trace = last.live_trace.is_some(),
            "Checking live_trace availability"
        );
        if let Some(ref mut trace) = last.live_trace {
            debug!("Adding tool call to live_trace");
            let index = trace.items.len();
            trace.add_tool_call(tool_call);
            trace.set_active_tool(index);
            new_tool_idx = Some(index);

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
                          event: &super::super::message_types::TraceEvent,
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
        } else {
            debug!("live_trace not available for tool call");
        }

        // Ensure new tool calls start collapsed (outside the mutable borrow of self.messages)
        if let Some(idx) = new_tool_idx {
            self.collapsed_tool_calls
                .entry((parent_idx, idx))
                .or_insert(true);
        }

        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Helper method to update a tool call by ID in the live trace.
    /// This works even after active_tool_index has been cleared.
    ///
    /// Delegates to `SystemTrace::update_tool_call` which uses a two-pass scan:
    ///
    /// 1. First pass (forward/FIFO): find the FIRST entry with matching ID
    ///    that is still in Running state — ensures results are matched to
    ///    the oldest pending call when duplicate IDs exist.
    /// 2. Fallback pass (reverse): find the LAST entry with matching ID
    ///    regardless of state — handles late-arriving updates.
    pub(super) fn update_tool_call_by_id<F>(&mut self, tool_id: &str, updater: F) -> bool
    where
        F: FnOnce(&mut ToolCallBlock),
    {
        let Some(idx) = self.parent_streaming_assistant_index() else {
            warn!("update_tool_call_by_id: No parent streaming assistant");
            return false;
        };
        let last_message = match self.messages.get_mut(idx) {
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

        if !trace.update_tool_call(tool_id, updater) {
            warn!(
                "update_tool_call_by_id: Tool call with id={} not found in trace items",
                tool_id
            );
            return false;
        }

        true
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
            tc.execution_engine =
                predict_execution_engine(&tc.tool_name, &arguments).or(tc.execution_engine);
            tc.input = arguments.clone();
        });

        // Update trace view - it will emit event if state changes
        if let Some(last) = self.parent_streaming_message_mut() {
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
        let is_denied = is_denial_result(&result);

        // Update trace by ID
        self.update_tool_call_by_id(&id, |tc| {
            tc.execution_engine = detect_execution_engine(&tc.tool_name, &result);
            tc.output = Some(result.clone());
            tc.state = if is_denied {
                ToolCallState::Error("Denied by user".to_string())
            } else {
                ToolCallState::Success
            };
        });

        // Update trace view - it will emit ToolCallStateChanged event automatically
        if let Some(last) = self.parent_streaming_message_mut() {
            if let Some(ref mut trace) = last.live_trace {
                let preview = trace.items.iter().find_map(|item| {
                    let TraceItem::ToolCall(tool) = item else {
                        return None;
                    };
                    if tool.id != id {
                        return None;
                    }
                    extract_table_preview(tool)
                });
                let chart_spec = trace.items.iter().find_map(|item| {
                    let TraceItem::ToolCall(tool) = item else {
                        return None;
                    };
                    if tool.id != id {
                        return None;
                    }
                    extract_chart_spec(tool)
                });
                let attachment_image = trace.items.iter().find_map(|item| {
                    let TraceItem::ToolCall(tool) = item else {
                        return None;
                    };
                    if tool.id != id {
                        return None;
                    }
                    attachment_image_path(tool)
                });
                trace.clear_active_tool();
                let trace_clone = trace.clone();
                if let Some(ref view_entity) = last.system_trace_view {
                    view_entity.update(cx, |view, cx| {
                        view.update_trace(trace_clone, cx); // This emits event!
                    });
                }
                if let Some(preview) = preview {
                    self.try_auto_open_query_table(&id, preview, cx);
                }
                if let Some(spec) = chart_spec {
                    self.try_auto_open_chart(&id, spec, cx);
                }
                if let Some(path) = attachment_image {
                    self.try_auto_open_image_artifact(&id, path, cx);
                }
            }
        }

        cx.notify();
        // No need for manual auto-expand - event handler does it
    }

    /// Handle tool call error event
    pub fn handle_tool_call_error(&mut self, id: String, error: String, cx: &mut Context<Self>) {
        // Update tool call state by ID
        self.update_tool_call_by_id(&id, |tc| {
            tc.state = ToolCallState::Error(error.clone());
        });

        // Update trace view - it will emit ToolCallStateChanged event automatically
        if let Some(last) = self.parent_streaming_message_mut() {
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
    pub(super) fn handle_trace_event(
        &mut self,
        event: &super::super::message_types::TraceEvent,
        cx: &mut Context<Self>,
    ) {
        use super::super::message_types::TraceEvent;

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
        if let Some(last) = self.parent_streaming_message_mut() {
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
                                  event: &super::super::message_types::TraceEvent,
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
        if let Some(last) = self.parent_streaming_message_mut() {
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
        if let Some(last) = self.parent_streaming_message_mut() {
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
        let last_message = match self.parent_streaming_message_mut() {
            Some(msg) => msg,
            None => return false,
        };

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
        if let Some(last) = self.parent_streaming_message_mut() {
            if let Some(ref mut trace) = last.live_trace {
                trace.clear_active_tool();
            }
        }

        cx.notify();
    }

    /// Handle approval decision from floating bar
    pub(super) fn handle_floating_approval(&mut self, approved: bool, cx: &mut Context<Self>) {
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

    /// Handle a clarifying-question request from the agent.
    ///
    /// Shows the popover above the chat input and records the questions in the
    /// live trace so they persist with the conversation.
    pub fn handle_clarification_requested(
        &mut self,
        id: String,
        questions: Vec<ClarifyingQuestion>,
        cx: &mut Context<Self>,
    ) {
        debug!(clarification_id = %id, questions = questions.len(), "UI: handle_clarification_requested called");

        // Answers from a previous request must not leak into this one; the
        // boxes are cleared on the next render, which has the `Window` that
        // `InputState::set_value` needs.
        self.clarification_inputs_dirty = true;

        if let Some(conversation_id) = self.conversation_id.clone() {
            self.pending_clarification = Some(PendingClarificationInfo {
                id: id.clone(),
                conversation_id,
                questions: questions.clone(),
                choices: HashMap::new(),
            });
        }

        let clarification = ClarificationBlock {
            id,
            questions,
            answers: Vec::new(),
            state: ClarificationState::Pending,
            created_at: std::time::SystemTime::now(),
        };

        if let Some(last) = self.parent_streaming_message_mut()
            && let Some(ref mut trace) = last.live_trace
        {
            let index = trace.items.len();
            trace.add_clarification(clarification);
            trace.set_active_tool(index);

            let trace_clone = trace.clone();
            if let Some(ref view_entity) = last.system_trace_view {
                view_entity.update(cx, |view, cx| {
                    view.update_trace(trace_clone, cx);
                    cx.notify();
                });
            }
        }

        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Drop the clarification popover, e.g. when the stream is cancelled.
    pub fn clear_pending_clarification(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_clarification.take() {
            self.record_clarification_answers(
                &pending.id,
                Vec::new(),
                ClarificationState::Cancelled,
                cx,
            );
            cx.notify();
        }
    }

    /// Record a clicked option for one question.
    pub(super) fn choose_clarification_option(
        &mut self,
        question_id: String,
        option_ix: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(ref mut pending) = self.pending_clarification {
            pending.choices.insert(question_id, ChosenOption(option_ix));
            cx.notify();
        }
    }

    /// Collect the answers the user has given so far.
    ///
    /// Free text always wins over a clicked option: if the user typed something
    /// after clicking, the typing is the later intent. Questions left entirely
    /// blank are omitted, so the model can see which ones went unanswered.
    pub(super) fn clarification_answers(&self, cx: &App) -> Vec<ClarificationAnswer> {
        let Some(ref pending) = self.pending_clarification else {
            return Vec::new();
        };

        pending
            .questions
            .iter()
            .enumerate()
            .filter_map(|(q_ix, question)| {
                let typed = self
                    .clarification_inputs
                    .get(q_ix)
                    .map(|slot| slot.read(cx).value().to_string())
                    .unwrap_or_default();
                let typed = typed.trim();

                if !typed.is_empty() {
                    return Some(ClarificationAnswer {
                        id: question.id.clone(),
                        answer: typed.to_string(),
                        custom: true,
                    });
                }

                let ChosenOption(ix) = pending.choices.get(&question.id).copied()?;
                let option = question.options.get(ix)?;
                Some(ClarificationAnswer {
                    id: question.id.clone(),
                    answer: option.clone(),
                    custom: false,
                })
            })
            .collect()
    }

    /// True once at least one question has an answer to send.
    pub(super) fn clarification_ready(&self, cx: &App) -> bool {
        !self.clarification_answers(cx).is_empty()
    }

    /// Send the collected answers back to the waiting `ask_user` tool.
    pub(super) fn submit_clarification(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.pending_clarification.as_ref().map(|p| p.id.clone()) else {
            return;
        };

        let answers = self.clarification_answers(cx);
        if answers.is_empty() {
            return;
        }

        // Hide the popover before resolving: the tool unblocks immediately and
        // the stream can push its next chunk straight away.
        self.pending_clarification = None;

        match cx.try_global::<chatty_core::models::ClarificationStore>() {
            Some(store) => {
                if !store.resolve(&id, answers.clone()) {
                    warn!(clarification_id = %id, "No pending clarification to resolve");
                }
            }
            None => {
                warn!(clarification_id = %id, "ClarificationStore global missing; answers dropped")
            }
        }

        self.record_clarification_answers(&id, answers, ClarificationState::Answered, cx);
        cx.notify();
    }

    /// Write the answers into the live trace so the transcript shows them.
    fn record_clarification_answers(
        &mut self,
        id: &str,
        answers: Vec<ClarificationAnswer>,
        state: ClarificationState,
        cx: &mut Context<Self>,
    ) {
        if let Some(last) = self.parent_streaming_message_mut()
            && let Some(ref mut trace) = last.live_trace
        {
            trace.resolve_clarification(id, answers, state);
            trace.clear_active_tool();

            let trace_clone = trace.clone();
            if let Some(ref view_entity) = last.system_trace_view {
                view_entity.update(cx, |view, cx| {
                    view.update_trace(trace_clone, cx);
                    cx.notify();
                });
            }
        }
    }

    /// Expand trace and scroll to approval for "View Details" button
    pub(super) fn expand_trace_to_approval(&mut self, cx: &mut Context<Self>) {
        trace!("expand_trace_to_approval called");

        if let Some(last) = self.parent_streaming_message_mut() {
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
}
