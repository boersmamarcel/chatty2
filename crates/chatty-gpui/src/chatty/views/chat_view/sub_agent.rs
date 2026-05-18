//! Sub-agent progress messages and other "auxiliary message" helpers.
//!
//! # What lives here
//!
//! - `start_sub_agent_progress` / `restore_sub_agent_progress` /
//!   `append_sub_agent_progress` / `finalize_sub_agent_progress` —
//!   manage a single in-flight sub-agent's collapsible progress trace.
//! - `add_info_message` — used by slash commands like `/cwd`, `/context`.
//! - `remove_last_assistant_message` — used by the regenerate flow.
//! - `clear_messages` — full reset, including the sub-agent index.
//!
//! These are grouped because they all *insert or remove* full
//! `DisplayMessage` entries (as opposed to mutating an in-progress
//! streaming message, which is what `handlers.rs` does).

use gpui::*;

use super::super::message_component::{DisplayMessage, MessageRole};
use super::super::message_types::{SystemTrace, ToolSource};
use super::super::trace_components::SystemTraceView;
use super::ChatView;

impl ChatView {
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
        self.sub_agent_progress_msg_idx = None;
        cx.notify();
    }

    /// Add the "Sub-agent" collapsible trace and record its index so that
    /// subsequent `append_sub_agent_progress` calls update it in-place.
    ///
    /// The progress is shown as a collapsible `ToolCallBlock` (Running state) so
    /// the user can expand it to see live stderr output while the sub-agent runs.
    pub fn start_sub_agent_progress(
        &mut self,
        prompt: &str,
        source: ToolSource,
        cx: &mut Context<Self>,
    ) {
        let trace = SystemTrace::new_sub_agent(prompt, source);
        self.restore_sub_agent_progress(trace, cx);
    }

    pub fn restore_sub_agent_progress(&mut self, trace: SystemTrace, cx: &mut Context<Self>) {
        let is_streaming = trace.active_tool_index.is_some();

        let trace_view = cx.new(|_cx| SystemTraceView::new(trace.clone()));

        self.messages.push(DisplayMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            is_streaming,
            system_trace_view: Some(trace_view),
            live_trace: if is_streaming { Some(trace) } else { None },
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        });

        let idx = self.messages.len() - 1;
        self.sub_agent_progress_msg_idx = is_streaming.then_some(idx);

        // Start expanded so live progress output is visible while the sub-agent runs.
        self.collapsed_tool_calls.insert((idx, 0), false);

        cx.notify();
        self.activate_sticky_scroll();
    }

    /// Append a sub-agent progress line to the tracked trace ToolCallBlock output.
    ///
    /// Called repeatedly while a sub-agent subprocess runs to accumulate live
    /// tool-call activity (e.g. "⟳ web_search", "✓ web_search") in the
    /// collapsible trace so the user can expand it to follow along.
    pub fn append_sub_agent_progress(&mut self, line: &str, cx: &mut Context<Self>) {
        let Some(idx) = self.sub_agent_progress_msg_idx else {
            return;
        };
        let Some(msg) = self.messages.get_mut(idx) else {
            return;
        };

        if let Some(ref mut trace) = msg.live_trace {
            trace.append_sub_agent_progress(line);
        }

        // Update the SystemTraceView entity so interleaved rendering picks up the change.
        let trace_clone = msg.live_trace.clone();
        let view_entity = msg.system_trace_view.clone();
        if let (Some(trace), Some(view_entity)) = (trace_clone, view_entity) {
            view_entity.update(cx, |view, cx| {
                view.update_trace(trace, cx);
                cx.notify();
            });
        }

        cx.notify();
    }

    /// Transition the sub-agent trace to its final state (Success or Error) and
    /// freeze the live_trace into the SystemTraceView entity.
    ///
    /// `result` is placed in the ToolCallBlock's `output` field so it appears
    /// in the expanded trace body instead of as a separate message.
    pub fn finalize_sub_agent_progress(
        &mut self,
        success: bool,
        result: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.sub_agent_progress_msg_idx else {
            return;
        };

        if let Some(msg) = self.messages.get_mut(idx) {
            if let Some(ref mut trace) = msg.live_trace {
                trace.finalize_sub_agent_progress(success, result);
            }

            // Push final trace state to the view entity.
            let trace_clone = msg.live_trace.clone();
            let view_entity = msg.system_trace_view.clone();
            if let (Some(trace), Some(view_entity)) = (trace_clone, view_entity) {
                view_entity.update(cx, |view, cx| {
                    view.update_trace(trace, cx);
                    cx.notify();
                });
            }

            // Auto-expand the trace so the result is immediately visible.
            self.collapsed_tool_calls.insert((idx, 0), false);

            msg.live_trace = None;
            msg.is_streaming = false;

            cx.notify();
        }

        self.sub_agent_progress_msg_idx = None;
    }

    /// Add an informational message that appears as an assistant response.
    /// Used for slash-command output such as /context and /cwd.
    pub fn add_info_message(&mut self, text: String, cx: &mut Context<Self>) {
        self.messages.push(DisplayMessage {
            role: MessageRole::Assistant,
            content: text,
            is_streaming: false,
            system_trace_view: None,
            live_trace: None,
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        });
        cx.notify();
        self.activate_sticky_scroll();
    }
}
