//! `SystemTraceView` per-item renderers.
//!
//! # What lives here
//!
//! - `render_header` — the collapsible "Tool calls" / "Sub-agent" header.
//! - `render_items` — the list of trace items (one card per item).
//! - `render_thinking_block` / `render_tool_call_block` /
//!   `render_approval_block` — the three card variants.
//!
//! All methods take `&self` or `&mut self` on `SystemTraceView`; they
//! are declared here in a second `impl SystemTraceView` block so the
//! main struct/lifecycle methods in `mod.rs` stay readable.

#![allow(clippy::collapsible_if)]

use crate::assets::CustomIcon;
use crate::chatty::models::execution_approval_store::{ApprovalDecision, ExecutionApprovalStore};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{ActiveTheme, Icon, Sizable, button::Button};
use std::time::Duration;

use super::super::message_types::{
    ApprovalState, ThinkingBlock, ToolCallBlock, ToolCallState, TraceItem,
};
use super::SystemTraceView;
use super::badges::{
    execution_engine_badge, is_code_execution_tool, render_execution_mode_badge,
    render_outline_badge, render_sub_agent_mode_badge, tool_source_badge,
};
use super::inline::{
    SelectableText, extract_command_display, extract_full_command, format_tool_output,
    render_code_run_input, render_full_command_box,
};

impl SystemTraceView {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Active status styling
        let has_active = self.trace.active_tool_index.is_some();

        let bg_color = if has_active {
            cx.theme().accent
        } else {
            cx.theme().muted
        };

        let collapse_icon = if self.is_collapsed { "▶" } else { "▼" };

        let border_color = cx.theme().border;
        let muted_text = cx.theme().muted_foreground;

        let mut header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .child(
                // Collapse/expand toggle button
                div()
                    .id("collapse-toggle")
                    .text_sm()
                    .text_color(muted_text)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _event, _window, cx| {
                            view.toggle_collapsed();
                            cx.notify();
                        }),
                    )
                    .child(collapse_icon),
            )
            .child(
                // Terminal prompt symbol
                div().text_sm().text_color(muted_text).child("$"),
            );

        // Show only the active step or the last completed step
        let display_step = if let Some(active_idx) = self.trace.active_tool_index {
            Some(active_idx)
        } else if !self.trace.items.is_empty() {
            Some(self.trace.items.len() - 1) // Show last step when complete
        } else {
            None
        };

        if let Some(idx) = display_step {
            if let Some(item) = self.trace.items.get(idx) {
                let step_num = idx + 1;
                let is_active = self.trace.active_tool_index == Some(idx);

                let (status, name, color) = match item {
                    TraceItem::ToolCall(tool_call) => match &tool_call.state {
                        ToolCallState::Running => (
                            "Running",
                            tool_call.display_name.as_str(),
                            cx.theme().primary,
                        ),
                        ToolCallState::Success => {
                            ("✓", tool_call.display_name.as_str(), cx.theme().accent)
                        }
                        ToolCallState::Error(_) => {
                            ("✗", tool_call.display_name.as_str(), cx.theme().ring)
                        }
                    },
                    TraceItem::Thinking(_) => {
                        if is_active {
                            ("Running", "thinking", cx.theme().primary)
                        } else {
                            ("✓", "analysis", cx.theme().accent)
                        }
                    }
                    TraceItem::ApprovalPrompt(approval) => match approval.state {
                        crate::chatty::views::message_types::ApprovalState::Pending => {
                            ("?", "approval", cx.theme().primary)
                        }
                        crate::chatty::views::message_types::ApprovalState::Approved => {
                            ("✓", "approved", cx.theme().accent)
                        }
                        crate::chatty::views::message_types::ApprovalState::Denied => {
                            ("✗", "denied", cx.theme().ring)
                        }
                    },
                };

                let mut step_container = div().flex().items_center().gap_1();
                let badge_text = cx.theme().primary_foreground;

                // Add animated indicator when active
                if is_active {
                    step_container = step_container.child(
                        div()
                            .id("active-indicator")
                            .child(
                                Icon::new(CustomIcon::Refresh)
                                    .size(px(12.0))
                                    .text_color(color),
                            )
                            .with_animation(
                                "active-indicator-pulse",
                                Animation::new(Duration::from_secs(2))
                                    .repeat()
                                    .with_easing(pulsating_between(0.4, 1.0)),
                                move |this, delta| this.opacity(delta),
                            ),
                    );
                }

                step_container = step_container
                    .child(
                        div()
                            .text_xs()
                            .text_color(color)
                            .child(format!("{} step {}", status, step_num)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_text)
                            .child(format!("({})", name)),
                    );

                // Data-egress badge for the active/last tool call
                if let TraceItem::ToolCall(tool_call) = item {
                    if let Some(engine) = tool_call.execution_engine {
                        if is_code_execution_tool(tool_call) {
                            step_container = step_container
                                .child(render_execution_mode_badge(engine, badge_text));
                        } else {
                            let (badge_text, badge_color) = execution_engine_badge(engine);
                            step_container =
                                step_container.child(render_outline_badge(badge_text, badge_color));
                        }
                    } else if let Some((badge_text, badge_color)) =
                        tool_source_badge(&tool_call.source)
                    {
                        step_container =
                            step_container.child(render_outline_badge(badge_text, badge_color));
                    }
                }

                header = header.child(step_container);
            }
        }

        header
    }

    /// Render individual trace items (always shown - terminal style)
    pub(super) fn render_items(&self, entity: WeakEntity<Self>, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .mt_2()
            .children(
                self.trace
                    .items
                    .iter()
                    .enumerate()
                    .map(move |(index, item)| match item {
                        TraceItem::Thinking(thinking) => self
                            .render_thinking_block(index, thinking, cx)
                            .into_any_element(),
                        TraceItem::ToolCall(tool_call) => self
                            .render_tool_call_block(index, tool_call, cx)
                            .into_any_element(),
                        TraceItem::ApprovalPrompt(approval) => self
                            .render_approval_block(index, approval, entity.clone(), cx)
                            .into_any_element(),
                    }),
            )
    }

    /// Render a thinking/reasoning block (terminal style)
    fn render_thinking_block(
        &self,
        index: usize,
        thinking: &ThinkingBlock,
        cx: &App,
    ) -> impl IntoElement {
        let is_active = self.trace.active_tool_index == Some(index);

        let (prefix, prefix_color) = if thinking.state.is_processing() || is_active {
            (">", cx.theme().primary)
        } else {
            ("✓", cx.theme().accent)
        };

        let muted_text = cx.theme().muted_foreground;
        let border_color = cx.theme().border;
        let text_color = cx.theme().foreground;

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                // Header line - shell-style prefix
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family("monospace")
                    .text_sm()
                    .child(
                        div()
                            .text_color(prefix_color)
                            .font_weight(FontWeight::BOLD)
                            .child(prefix),
                    )
                    .child(
                        div()
                            .text_color(muted_text)
                            .child(if thinking.state.is_processing() {
                                "thinking..."
                            } else {
                                "analysis"
                            }),
                    )
                    .when_some(thinking.duration, |this, duration| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted_text)
                                .child(format!("({:.1}s)", duration.as_secs_f32())),
                        )
                    }),
            )
            .child(
                // Content with left border (terminal output style)
                div()
                    .ml_4()
                    .pl_3()
                    .border_l_2()
                    .border_color(border_color)
                    .font_family("monospace")
                    .text_sm()
                    .text_color(text_color)
                    .child(thinking.content.clone()),
            )
    }

    /// Render a tool call block (terminal style)
    fn render_tool_call_block(
        &self,
        index: usize,
        tool_call: &ToolCallBlock,
        cx: &App,
    ) -> impl IntoElement {
        let is_running = matches!(tool_call.state, ToolCallState::Running);

        let (prefix, prefix_color, state_label) = match &tool_call.state {
            ToolCallState::Running => (">", cx.theme().primary, "running"),
            ToolCallState::Success => ("✓", cx.theme().accent, "success"),
            ToolCallState::Error(_) => ("✗", cx.theme().ring, "error"),
        };

        let muted_text = cx.theme().muted_foreground;
        let border_color = cx.theme().border;
        let text_color = cx.theme().foreground;
        let panel_bg = cx.theme().muted;
        let badge_text = cx.theme().primary_foreground;

        let badge = div()
            .id(ElementId::Name(format!("tool-badge-{}", index).into()))
            .text_xs()
            .px_2()
            .py(px(0.5))
            .rounded_sm()
            .bg(prefix_color)
            .text_color(badge_text)
            .child(state_label);

        let mut container = div().flex().flex_col().gap_1().child(
            // Command invocation line
            div()
                .flex()
                .items_center()
                .gap_2()
                .font_family("monospace")
                .text_sm()
                .child(
                    div()
                        .text_color(prefix_color)
                        .font_weight(FontWeight::BOLD)
                        .child(prefix),
                )
                .child(
                    div()
                        .text_color(text_color)
                        .font_weight(FontWeight::BOLD)
                        .child(extract_command_display(tool_call)),
                )
                .map(|this| {
                    let this = if tool_call.tool_name == "sub_agent" {
                        this.child(render_sub_agent_mode_badge(&tool_call.source, badge_text))
                    } else if is_code_execution_tool(tool_call) {
                        if let Some(engine) = tool_call.execution_engine {
                            this.child(render_execution_mode_badge(engine, badge_text))
                        } else {
                            this
                        }
                    } else {
                        this
                    };

                    let this = if is_running {
                        this.child(
                            badge.with_animation(
                                ElementId::Name(format!("tool-badge-pulse-{}", index).into()),
                                Animation::new(Duration::from_secs(2))
                                    .repeat()
                                    .with_easing(pulsating_between(0.4, 1.0)),
                                |el, delta| el.opacity(delta),
                            ),
                        )
                    } else {
                        this.child(badge)
                    };

                    if tool_call.tool_name != "sub_agent" && !is_code_execution_tool(tool_call) {
                        if let Some(engine) = tool_call.execution_engine {
                            let (badge_text, badge_color) = execution_engine_badge(engine);
                            this.child(render_outline_badge(badge_text, badge_color))
                        } else if let Some((badge_text, badge_color)) =
                            tool_source_badge(&tool_call.source)
                        {
                            this.child(render_outline_badge(badge_text, badge_color))
                        } else {
                            this
                        }
                    } else {
                        this
                    }
                })
                .when_some(tool_call.duration, |this, duration| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(muted_text)
                            .child(format!("({:.1}s)", duration.as_secs_f32())),
                    )
                }),
        );

        // Show runnable code with syntax highlighting when available.
        if let Some(code_block) = render_code_run_input(tool_call, index) {
            container = container.child(
                div()
                    .ml_4()
                    .pl_3()
                    .border_l_2()
                    .border_color(border_color)
                    .child(code_block),
            );
        } else {
            let full_command = extract_full_command(tool_call);
            if full_command.chars().count() > 80 {
                container = container.child(
                    div()
                        .ml_4()
                        .pl_3()
                        .border_l_2()
                        .border_color(border_color)
                        .child(render_full_command_box(full_command, panel_bg, text_color)),
                );
            }
        }

        // Output section (if available)
        if let Some(output) = tool_call
            .output
            .as_ref()
            .or(tool_call.output_preview.as_ref())
        {
            let formatted_output = format_tool_output(output);
            container = container.child(
                div()
                    .ml_4()
                    .pl_3()
                    .border_l_2()
                    .border_color(border_color)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .text_color(muted_text)
                            .child("output:"),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .px_2()
                            .py_1()
                            .bg(panel_bg)
                            .rounded_sm()
                            .text_color(text_color)
                            .child(SelectableText::new(
                                ElementId::Name(format!("tool-output-{}", index).into()),
                                formatted_output,
                            )),
                    ),
            );
        }

        // Error section (if error state)
        if let ToolCallState::Error(error) = &tool_call.state {
            let error_color = cx.theme().ring;
            let error_bg = cx.theme().accent;
            let error_border = cx.theme().ring;

            container = container.child(
                div()
                    .ml_4()
                    .pl_3()
                    .border_l_2()
                    .border_color(error_border)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .text_color(error_color)
                            .font_weight(FontWeight::BOLD)
                            .child("error:"),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .px_2()
                            .py_1()
                            .bg(error_bg)
                            .rounded_sm()
                            .text_color(error_color)
                            .child(SelectableText::new(
                                ElementId::Name(format!("tool-error-{}", index).into()),
                                error.clone(),
                            )),
                    ),
            );
        }

        container
    }

    /// Render an approval prompt block (for code execution)
    fn render_approval_block(
        &self,
        _index: usize,
        approval: &crate::chatty::views::message_types::ApprovalBlock,
        entity: WeakEntity<Self>,
        cx: &App,
    ) -> impl IntoElement {
        let (prefix, prefix_color, state_label) = match &approval.state {
            ApprovalState::Pending => ("?", cx.theme().primary, "awaiting approval"),
            ApprovalState::Approved => ("✓", cx.theme().accent, "approved"),
            ApprovalState::Denied => ("✗", cx.theme().ring, "denied"),
        };

        let is_pending = matches!(approval.state, ApprovalState::Pending);
        let muted_text = cx.theme().muted_foreground;
        let border_color = cx.theme().border;
        let text_color = cx.theme().foreground;
        let panel_bg = cx.theme().muted;
        let badge_text = cx.theme().primary_foreground;

        div()
            .flex()
            .flex_col()
            .gap_2()
            // Header with status
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family("monospace")
                    .text_sm()
                    .child(
                        div()
                            .text_color(prefix_color)
                            .font_weight(FontWeight::BOLD)
                            .child(prefix),
                    )
                    .child(
                        div()
                            .text_color(text_color)
                            .font_weight(FontWeight::BOLD)
                            .child("Execution approval requested"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .px_2()
                            .py(px(0.5))
                            .rounded_sm()
                            .bg(prefix_color)
                            .text_color(badge_text)
                            .child(state_label),
                    ),
            )
            // Command display
            .child(
                div()
                    .ml_4()
                    .pl_3()
                    .border_l_2()
                    .border_color(border_color)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .text_color(muted_text)
                            .child("command:"),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_sm()
                            .px_2()
                            .py_1()
                            .bg(panel_bg)
                            .rounded_sm()
                            .text_color(text_color)
                            .child(approval.command.clone()),
                    )
                    .child(div().text_xs().text_color(muted_text).child(
                        if approval.is_sandboxed {
                            "🔒 sandboxed execution"
                        } else {
                            "⚠️  unsandboxed execution"
                        },
                    )),
            )
            // Buttons (only when pending)
            .when(is_pending, |this| {
                let approval_id = approval.id.clone();
                let entity_for_approve = entity.clone();
                let entity_for_deny = entity.clone();

                this.child(
                    div()
                        .ml_4()
                        .pl_3()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new(ElementId::Name(format!("approve-{}", approval.id).into()))
                                .label("Approve")
                                .small()
                                .on_click({
                                    let id = approval_id.clone();
                                    move |_event, _window, cx| {
                                        if let Some(store) =
                                            cx.try_global::<ExecutionApprovalStore>()
                                        {
                                            store.resolve(&id, ApprovalDecision::Approved);

                                            // Update UI state
                                            if let Some(entity) = entity_for_approve.upgrade() {
                                                entity.update(cx, |view, cx| {
                                                    // Find and update the approval in the trace
                                                    for item in &mut view.trace.items {
                                                        if let TraceItem::ApprovalPrompt(approval) =
                                                            item
                                                        {
                                                            if approval.id == id {
                                                                approval.state =
                                                                    ApprovalState::Approved;
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    cx.notify();
                                                });
                                            }
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new(ElementId::Name(format!("deny-{}", approval.id).into()))
                                .label("Deny")
                                .small()
                                .on_click({
                                    let id = approval_id;
                                    move |_event, _window, cx| {
                                        if let Some(store) =
                                            cx.try_global::<ExecutionApprovalStore>()
                                        {
                                            store.resolve(&id, ApprovalDecision::Denied);

                                            // Update UI state
                                            if let Some(entity) = entity_for_deny.upgrade() {
                                                entity.update(cx, |view, cx| {
                                                    // Find and update the approval in the trace
                                                    for item in &mut view.trace.items {
                                                        if let TraceItem::ApprovalPrompt(approval) =
                                                            item
                                                        {
                                                            if approval.id == id {
                                                                approval.state =
                                                                    ApprovalState::Denied;
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    cx.notify();
                                                });
                                            }
                                        }
                                    }
                                }),
                        ),
                )
            })
    }
}
