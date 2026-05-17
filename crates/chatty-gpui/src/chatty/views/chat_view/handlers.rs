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
    ApprovalBlock, ApprovalState, ThinkingBlock, ThinkingState, ToolCallBlock, ToolCallState,
    ToolSource, TraceItem, classify_initial_execution_engine, detect_execution_engine,
    friendly_tool_name, is_denial_result, predict_execution_engine,
};
use super::super::trace_components::SystemTraceView;
use super::{ChatView, PendingApprovalInfo};

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

        // Capture current message content as "text_before" for interleaved rendering
        let text_before = self
            .messages
            .last()
            .map(|msg| msg.content.clone())
            .unwrap_or_default();

        let had_trace_view = self
            .messages
            .last()
            .and_then(|m| m.system_trace_view.as_ref())
            .is_some();
        let live_trace_items = self
            .messages
            .last()
            .and_then(|m| m.live_trace.as_ref())
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
        let msg_count = self.messages.len();
        let mut new_tool_idx: Option<usize> = None;
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
                }
            } else {
                debug!("live_trace not available for tool call");
            }
        } else {
            debug!("Last message is not streaming");
        }

        // Ensure new tool calls start collapsed (outside the mutable borrow of self.messages)
        if let Some(idx) = new_tool_idx {
            self.collapsed_tool_calls
                .entry((msg_count - 1, idx))
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

    /// Expand trace and scroll to approval for "View Details" button
    pub(super) fn expand_trace_to_approval(&mut self, cx: &mut Context<Self>) {
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
}
