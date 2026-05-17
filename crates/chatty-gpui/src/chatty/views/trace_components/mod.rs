//! Tool-call trace UI components — the expandable cards that visualize an
//! assistant's tool calls inline in the chat view.
//!
//! # What lives here
//!
//! - `TraceComponent` and its subviews — header row, args summary,
//!   expandable JSON, result preview, error banner, approval prompt, …
//! - Visual treatment for each tool kind (shell, filesystem, MCP, etc.).
//! - User interactions (approve / deny via `ExecutionApprovalStore`,
//!   copy, expand/collapse).
//!
//! # What does NOT live here
//!
//! - The underlying trace data — `chatty_core::models::message_types::ToolCall`
//!   and friends.
//! - Code-block / diff rendering — `code_block_component`, `diff_view_component`.
//! - The actual tool implementations — `chatty_core::tools::*`.
//!
//! See `docs/rendering-system.md` for how these components fit into the
//! overall message rendering pipeline.

#![allow(clippy::collapsible_if)]

mod badges;
mod blocks;
mod inline;

// Re-export the public API so external callers (chat_view,
// message_component) see the same `trace_components::*` namespace as
// before the split.
pub use inline::render_tool_call_inline;

use crate::assets::CustomIcon;
use crate::chatty::models::execution_approval_store::{ApprovalDecision, ExecutionApprovalStore};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{ActiveTheme, Icon, Sizable, button::Button, text::TextView};
use std::time::Duration;

use super::code_block_component::CodeBlockComponent;
use super::diff_view_component::DiffViewComponent;
use super::message_types::{
    ApprovalState, ExecutionEngine, SystemTrace, ThinkingBlock, ToolCallBlock, ToolCallState,
    ToolSource, TraceEvent, TraceItem,
};
use gpui::EventEmitter;

pub struct SystemTraceView {
    trace: SystemTrace,
    is_collapsed: bool,
}

impl EventEmitter<TraceEvent> for SystemTraceView {}

impl SystemTraceView {
    pub fn new(trace: SystemTrace) -> Self {
        Self {
            trace,
            is_collapsed: true,
        }
    }

    /// Allow updating trace during streaming and emit events for changes
    pub fn update_trace(&mut self, new_trace: SystemTrace, cx: &mut Context<Self>) {
        // Compare items at the SAME INDEX position (not by ID!)
        // This ensures we're comparing the same tool call at different stages,
        // not matching against old tool calls from previous turns

        for (index, new_item) in new_trace.items.iter().enumerate() {
            // Get the corresponding old item at the same index (if it exists)
            let old_item = self.trace.items.get(index);

            match (new_item, old_item) {
                (TraceItem::ToolCall(new_tc), Some(TraceItem::ToolCall(old_tc))) => {
                    // Same tool call, check for state changes
                    if new_tc.state != old_tc.state {
                        cx.emit(TraceEvent::ToolCallStateChanged {
                            tool_id: new_tc.id.clone(),
                            old_state: old_tc.state.clone(),
                            new_state: new_tc.state.clone(),
                        });
                    }

                    // Check for output received
                    if new_tc.output.is_some() && old_tc.output.is_none() {
                        cx.emit(TraceEvent::ToolCallOutputReceived {
                            tool_id: new_tc.id.clone(),
                            has_output: true,
                        });
                    }
                }
                (TraceItem::Thinking(new_tb), Some(TraceItem::Thinking(old_tb))) => {
                    if new_tb.state != old_tb.state {
                        cx.emit(TraceEvent::ThinkingStateChanged {
                            old_state: old_tb.state.clone(),
                            new_state: new_tb.state.clone(),
                        });
                    }
                }
                // New item with no old item at this index - no state change to report
                _ => {}
            }
        }

        // Update the trace
        self.trace = new_trace;
    }

    /// Toggle the collapsed state
    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.is_collapsed = collapsed;
    }

    /// Get a reference to the trace (for interleaved rendering)
    pub fn get_trace(&self) -> &SystemTrace {
        &self.trace
    }
}

impl Render for SystemTraceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();

        let mut container = div()
            .flex()
            .flex_col()
            .gap_2()
            .mt_2()
            .mb_2()
            .ml_4() // Indent from main message
            .child(self.render_header(cx));

        // Only show items if not collapsed
        if !self.is_collapsed {
            container = container.child(self.render_items(entity, cx));
        }

        container
    }
}

