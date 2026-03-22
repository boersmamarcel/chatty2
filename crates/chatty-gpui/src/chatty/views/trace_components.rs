#![allow(clippy::collapsible_if)]

use crate::assets::CustomIcon;
use crate::chatty::models::execution_approval_store::{ApprovalDecision, ExecutionApprovalStore};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{
    ActiveTheme, Icon, Sizable,
    button::{Button, ButtonVariants},
    text::TextView,
};
use std::time::Duration;

use super::diff_view_component::DiffViewComponent;
use super::message_types::{
    ApprovalState, SystemTrace, ThinkingBlock, ToolCallBlock, ToolCallState, TraceEvent, TraceItem,
};
use gpui::EventEmitter;

/// Component for rendering the system trace container
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

    /// Render the trace container header with active status
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

                header = header.child(step_container);
            }
        }

        header
    }

    /// Render individual trace items (always shown - terminal style)
    fn render_items(&self, entity: WeakEntity<Self>, cx: &App) -> impl IntoElement {
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
                    if is_running {
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

        // Show full command when the header was truncated
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

        // Website preview card for browse tool
        if is_browse_tool(&tool_call.tool_name) {
            if let Some(preview) = extract_browse_preview(tool_call) {
                let link_url = preview.url.clone();
                container = container.child(
                    div()
                        .ml_4()
                        .pl_3()
                        .border_l_2()
                        .border_color(border_color)
                        .child(render_website_preview_card(index, &preview, link_url, cx)),
                );
            }
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

/// Public function to render a single tool call inline (for interleaved content)
#[allow(clippy::too_many_arguments)]
pub fn render_tool_call_inline<F, D>(
    tool_call: &ToolCallBlock,
    message_index: usize,
    tool_index: usize,
    collapsed: bool,
    on_toggle: F,
    diff_expanded: bool,
    on_expand_diff: D,
    cx: &App,
) -> impl IntoElement
where
    F: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    D: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
{
    use tracing::debug;

    debug!(
        "Rendering tool call: display_name={}, state={:?}",
        tool_call.display_name, tool_call.state
    );

    let is_running = matches!(tool_call.state, ToolCallState::Running);

    let (prefix, prefix_color, state_label) = match &tool_call.state {
        ToolCallState::Running => (">", cx.theme().primary, "running"),
        ToolCallState::Success => ("✓", gpui::green(), "success"),
        ToolCallState::Error(_) => ("✗", cx.theme().ring, "error"),
    };

    let muted_text = cx.theme().muted_foreground;
    let text_color = cx.theme().foreground;
    let panel_bg = cx.theme().muted;
    let badge_text = cx.theme().primary_foreground;

    let inline_badge = div()
        .id(ElementId::Name(
            format!("inline-badge-{}-{}", message_index, tool_index).into(),
        ))
        .text_xs()
        .px_2()
        .py(px(0.5))
        .rounded_sm()
        .bg(prefix_color)
        .text_color(badge_text)
        .flex_shrink_0()
        .child(state_label);

    // Compact clickable header (always visible)
    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .overflow_hidden()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            on_toggle(event, window, cx);
        })
        .child(
            div()
                .text_color(muted_text)
                .flex_shrink_0()
                .child(if collapsed { "▶" } else { "▼" }),
        )
        .child(
            div()
                .text_color(prefix_color)
                .font_weight(FontWeight::BOLD)
                .flex_shrink_0()
                .child(prefix),
        )
        .child(
            div()
                .text_color(text_color)
                .font_weight(FontWeight::BOLD)
                .font_family("monospace")
                .text_sm()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(format_tool_call_header(tool_call)),
        )
        .map(|this| {
            if is_running {
                this.child(
                    inline_badge.with_animation(
                        ElementId::Name(
                            format!("inline-badge-pulse-{}-{}", message_index, tool_index).into(),
                        ),
                        Animation::new(Duration::from_secs(2))
                            .repeat()
                            .with_easing(pulsating_between(0.4, 1.0)),
                        |el, delta| el.opacity(delta),
                    ),
                )
            } else {
                this.child(inline_badge)
            }
        })
        .when_some(tool_call.duration, |this, duration| {
            this.child(
                div()
                    .text_xs()
                    .text_color(muted_text)
                    .flex_shrink_0()
                    .child(format!("({:.1}s)", duration.as_secs_f32())),
            )
        });

    // Build accordion content children (what shows when expanded)
    let mut content_children = Vec::new();

    // Show full command when the header was truncated
    let full_command = extract_full_command(tool_call);
    if full_command.chars().count() > 80 {
        content_children.push(render_full_command_box(full_command, panel_bg, text_color));
    }

    // For apply_diff tool calls with success, render a visual diff view
    let has_diff_view = if tool_call.tool_name == "apply_diff"
        && matches!(tool_call.state, ToolCallState::Success)
    {
        if let Some(diff_view) = try_build_diff_view(
            &tool_call.input,
            message_index,
            tool_index,
            diff_expanded,
            on_expand_diff,
        ) {
            content_children.push(diff_view);
            true
        } else {
            false
        }
    } else {
        false
    };

    // Add output section if available (skip for apply_diff with diff view)
    if !has_diff_view {
        if let Some(output) = tool_call
            .output
            .as_ref()
            .or(tool_call.output_preview.as_ref())
        {
            let formatted_output = format_tool_output(output);
            content_children.push(
                div()
                    .font_family("monospace")
                    .text_xs()
                    .px_2()
                    .py_1()
                    .bg(panel_bg)
                    .rounded_sm()
                    .text_color(text_color)
                    .child(SelectableText::new(
                        ElementId::Name(
                            format!("inline-tool-output-{}-{}", message_index, tool_index).into(),
                        ),
                        formatted_output,
                    ))
                    .into_any_element(),
            );
        } else if matches!(tool_call.state, ToolCallState::Running) {
            // Show "Running..." for running tools
            content_children.push(
                div()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(muted_text)
                    .child("Running...")
                    .into_any_element(),
            );
        }
    }

    // Website preview card for browse tool (inline rendering)
    if is_browse_tool(&tool_call.tool_name) {
        if let Some(preview) = extract_browse_preview(tool_call) {
            let link_url = preview.url.clone();
            content_children.push(
                render_website_preview_card(
                    message_index * 1000 + tool_index,
                    &preview,
                    link_url,
                    cx,
                )
                .into_any_element(),
            );
        }
    }

    // Add error section if error state
    if let ToolCallState::Error(error) = &tool_call.state {
        let error_color = cx.theme().ring;
        let error_bg = cx.theme().accent;

        content_children.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_family("monospace")
                        .text_xs()
                        .text_color(error_color)
                        .font_weight(FontWeight::BOLD)
                        .child("Error:"),
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
                            ElementId::Name(
                                format!("inline-tool-error-{}-{}", message_index, tool_index)
                                    .into(),
                            ),
                            error.clone(),
                        )),
                )
                .into_any_element(),
        );
    }

    // Return header + conditionally visible content
    let mut result = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .bg(panel_bg.opacity(0.3))
        .child(header);

    // Always-visible compact URL row for browse tools
    if is_browse_tool(&tool_call.tool_name) {
        if let Some(url) = extract_browse_url(tool_call) {
            let link_url = url.clone();
            let domain = extract_domain(&url);
            result = result.child(
                div()
                    .id(ElementId::Name(
                        format!("open-browse-inline-{}-{}", message_index, tool_index).into(),
                    ))
                    .pl_6()
                    .pt(px(2.0))
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, _cx| {
                        let url = if link_url.starts_with("http://")
                            || link_url.starts_with("https://")
                        {
                            link_url.clone()
                        } else {
                            format!("https://{}", link_url)
                        };
                        if let Err(e) = open::that_detached(&url) {
                            tracing::warn!(url = %url, error = %e, "Failed to open URL in browser");
                        }
                    })
                    .child(
                        Icon::new(CustomIcon::Earth)
                            .size(px(12.0))
                            .text_color(muted_text),
                    )
                    .child(div().text_xs().text_color(muted_text).child(domain))
                    .child(
                        Icon::new(CustomIcon::ExternalLink)
                            .size(px(10.0))
                            .text_color(muted_text),
                    ),
            );
        }
    }

    result.when(!collapsed && !content_children.is_empty(), |this| {
        this.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pl_4() // Indent content slightly
                .children(content_children),
        )
    })
}

/// Try to build a diff view from apply_diff tool input JSON.
/// Returns None if parsing fails.
fn try_build_diff_view(
    input_json: &str,
    message_index: usize,
    tool_index: usize,
    diff_expanded: bool,
    on_expand_diff: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> Option<AnyElement> {
    #[derive(serde::Deserialize)]
    struct ApplyDiffInput {
        path: String,
        old_content: String,
        new_content: String,
    }

    let args: ApplyDiffInput = serde_json::from_str(input_json).ok()?;

    Some(
        DiffViewComponent::new(
            args.old_content,
            args.new_content,
            args.path,
            message_index,
            tool_index,
            diff_expanded,
        )
        .on_expand(on_expand_diff)
        .into_any_element(),
    )
}

/// Render the full command text box (used when the header was truncated)
fn render_full_command_box(
    full_command: String,
    panel_bg: Hsla,
    text_color: Hsla,
) -> gpui::AnyElement {
    div()
        .font_family("monospace")
        .text_xs()
        .px_2()
        .py_1()
        .bg(panel_bg)
        .rounded_sm()
        .text_color(text_color)
        .child(full_command)
        .into_any_element()
}

/// Format the inline header text for a tool call.
///
/// Most tools show `$ <command>` (shell-style), but internet and memory tools use their
/// friendly name as a prefix (e.g. "Searching online: rust async patterns").
fn format_tool_call_header(tool_call: &ToolCallBlock) -> String {
    let detail = extract_command_display(tool_call);

    match tool_call.tool_name.as_str() {
        "remember" | "search_memory" | "search_web" | "fetch" | "sub_agent" => {
            // Use the friendly display_name as prefix with the detail
            format!("{}: {}", tool_call.display_name, detail)
        }
        _ => format!("$ {}", detail),
    }
}

/// Extract a user-friendly display string from tool call input (truncated for headers)
fn extract_command_display(tool_call: &ToolCallBlock) -> String {
    let full = extract_full_command(tool_call);
    if full.chars().count() > 80 {
        let truncated: String = full.chars().take(77).collect();
        format!("{}...", truncated)
    } else {
        full
    }
}

/// Extract the full, untruncated command string from tool call input
fn extract_full_command(tool_call: &ToolCallBlock) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&tool_call.input) {
        // For shell_execute tool: extract "command" field
        if tool_call.tool_name == "shell_execute" {
            if let Some(command) = json.get("command").and_then(|v| v.as_str()) {
                return command.to_string();
            }
        }

        // For execute_code: show language prefix + full code
        if tool_call.tool_name == "execute_code" {
            let language = json.get("language").and_then(|v| v.as_str()).unwrap_or("?");
            let code = json.get("code").and_then(|v| v.as_str()).unwrap_or("");
            return format!("[{}] {}", language, code);
        }

        // For remember tool: prefer title, fall back to truncated content
        if tool_call.tool_name == "remember" {
            if let Some(title) = json.get("title").and_then(|v| v.as_str()) {
                return title.to_string();
            }
            if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                let truncated: String = content.chars().take(80).collect();
                if content.len() > 80 {
                    return format!("{}...", truncated);
                }
                return truncated;
            }
        }

        // For search_memory: extract query
        if tool_call.tool_name == "search_memory" {
            if let Some(query) = json.get("query").and_then(|v| v.as_str()) {
                return query.to_string();
            }
        }

        // For search_web: extract the search query
        if tool_call.tool_name == "search_web" {
            if let Some(query) = json.get("query").and_then(|v| v.as_str()) {
                return query.to_string();
            }
        }

        // For browse: extract the URL
        if tool_call.tool_name == "browse" {
            if let Some(url) = json.get("url").and_then(|v| v.as_str()) {
                return url.to_string();
            }
        }

        // For fetch: extract the URL
        if tool_call.tool_name == "fetch" {
            if let Some(url) = json.get("url").and_then(|v| v.as_str()) {
                return url.to_string();
            }
        }

        // For sub_agent: extract the task prompt
        if tool_call.tool_name == "sub_agent" {
            if let Some(task) = json.get("task").and_then(|v| v.as_str()) {
                return task.to_string();
            }
        }

        // For other tools: try to extract a "query" or "path" or first string field
        if let Some(query) = json.get("query").and_then(|v| v.as_str()) {
            return query.to_string();
        }

        if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
            return path.to_string();
        }
    }

    // Fallback to display_name if we can't extract anything
    tool_call.display_name.clone()
}

/// Format tool call output for display (extract useful info from JSON)
fn format_tool_output(output: &str) -> String {
    // Try to parse as JSON
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        // If it's an object with common result fields, extract them
        if let Some(obj) = json.as_object() {
            // browse output: show a compact text summary instead of full JSON
            if let Some(snapshot) = obj.get("snapshot").and_then(|v| v.as_object()) {
                let mut parts: Vec<String> = Vec::new();
                if let Some(title) = snapshot.get("title").and_then(|v| v.as_str()) {
                    if !title.is_empty() {
                        parts.push(format!("Title: {}", title));
                    }
                }
                if let Some(text) = snapshot.get("text_content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        let truncated: String = text.chars().take(300).collect();
                        let suffix = if text.len() > 300 { "…" } else { "" };
                        parts.push(format!("{}{}", truncated, suffix));
                    }
                }
                let element_count = snapshot
                    .get("elements")
                    .and_then(|v| v.as_array())
                    .map_or(0, |a| a.len());
                let link_count = snapshot
                    .get("links")
                    .and_then(|v| v.as_array())
                    .map_or(0, |a| a.len());
                if element_count > 0 || link_count > 0 {
                    parts.push(format!(
                        "[{} interactive elements, {} links]",
                        element_count, link_count
                    ));
                }
                return if parts.is_empty() {
                    "(empty page)".to_string()
                } else {
                    parts.join("\n")
                };
            }

            // execute_code output: show stdout + stderr + exit_code + port_mappings
            if obj.contains_key("exit_code") && obj.contains_key("timed_out") {
                let mut parts: Vec<String> = Vec::new();

                if let Some(stdout) = obj.get("stdout").and_then(|v| v.as_str()) {
                    if !stdout.is_empty() {
                        parts.push(stdout.to_string());
                    }
                }
                if let Some(stderr) = obj.get("stderr").and_then(|v| v.as_str()) {
                    if !stderr.is_empty() {
                        parts.push(format!("[stderr]\n{}", stderr));
                    }
                }
                if let Some(true) = obj.get("timed_out").and_then(|v| v.as_bool()) {
                    parts.push("[timed out]".to_string());
                }
                if let Some(code) = obj.get("exit_code").and_then(|v| v.as_i64()) {
                    if code != 0 {
                        parts.push(format!("[exit code: {}]", code));
                    }
                }
                if let Some(ports) = obj.get("port_mappings").and_then(|v| v.as_object()) {
                    if !ports.is_empty() {
                        let mappings: Vec<String> = ports
                            .iter()
                            .filter_map(|(k, v)| v.as_u64().map(|hp| format!("{}→{}", k, hp)))
                            .collect();
                        parts.push(format!("[ports: {}]", mappings.join(", ")));
                    }
                }

                return if parts.is_empty() {
                    "(no output)".to_string()
                } else {
                    parts.join("\n")
                };
            }

            // Check for common output patterns (in order of priority)
            if let Some(stdout) = obj.get("stdout").and_then(|v| v.as_str()) {
                return stdout.to_string();
            }

            if let Some(result) = obj.get("result").and_then(|v| v.as_str()) {
                return result.to_string();
            }

            if let Some(output) = obj.get("output").and_then(|v| v.as_str()) {
                return output.to_string();
            }

            if let Some(message) = obj.get("message").and_then(|v| v.as_str()) {
                return message.to_string();
            }

            if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                return content.to_string();
            }

            // If JSON object has just one string field, return it
            if obj.len() == 1 {
                if let Some((_key, value)) = obj.iter().next() {
                    if let Some(s) = value.as_str() {
                        return s.to_string();
                    }
                }
            }

            // Pretty print the JSON if it's a structured object
            if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                return pretty;
            }
        }

        // If it's a plain string value, unwrap it
        if let Some(s) = json.as_str() {
            return s.to_string();
        }
    }

    // Return as-is if not JSON or can't extract anything useful
    output.to_string()
}

/// A simple `RenderOnce` wrapper that renders plain text using `TextView` with
/// text selection enabled. Using a struct defers the `window`/`cx` requirement
/// of `TextView::markdown` to render time, so callers don't need `&mut Window`.
#[derive(IntoElement)]
struct SelectableText {
    id: ElementId,
    text: SharedString,
}

impl SelectableText {
    fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

impl RenderOnce for SelectableText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Escape markdown-significant characters so raw tool output (JSON, shell
        // output, etc.) is rendered verbatim rather than interpreted as markdown.
        let escaped = escape_markdown(&self.text);
        TextView::markdown(self.id, escaped, window, cx).selectable(true)
    }
}

/// Escape characters that carry special meaning in CommonMark so that arbitrary
/// plain text is rendered literally when fed to a markdown renderer.
fn escape_markdown(s: &str) -> SharedString {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '>' | '~' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    SharedString::from(out)
}

// ── Browse tool website preview ─────────────────────────────────────────────

/// Data extracted from a browse tool output for the preview card.
struct BrowsePreview {
    url: String,
    title: String,
    description: Option<String>,
    screenshot_path: Option<String>,
}

/// Extract the domain name from a URL (e.g. "https://nos.nl/path" → "nos.nl").
fn extract_domain(url: &str) -> String {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

/// Check whether a tool name is a browser navigation tool.
fn is_browse_tool(tool_name: &str) -> bool {
    tool_name == "browse"
}

/// Extract the URL from a browse tool's input JSON.
fn extract_browse_url(tool_call: &ToolCallBlock) -> Option<String> {
    if let Some(output) = tool_call.output.as_ref() {
        // Try JSON first (serialized BrowseOutput)
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
            let snapshot = json.get("snapshot").unwrap_or(&json);
            if let Some(url) = snapshot.get("url").and_then(|v| v.as_str()) {
                if !url.is_empty() {
                    return Some(url.to_string());
                }
            }
        }
        // Try Display-formatted text: "URL: https://..."
        for line in output.lines() {
            if let Some(url) = line.strip_prefix("URL: ") {
                let url = url.trim();
                if !url.is_empty() {
                    return Some(url.to_string());
                }
            }
        }
    }
    // Fall back to the input URL
    let json: serde_json::Value = serde_json::from_str(&tool_call.input).ok()?;
    json.get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract preview data from a browse tool's output (JSON or Display text).
fn extract_browse_preview(tool_call: &ToolCallBlock) -> Option<BrowsePreview> {
    let output = tool_call.output.as_ref()?;

    // Try JSON first (serialized BrowseOutput)
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        let snapshot = json.get("snapshot").unwrap_or(&json);

        if let Some(url) = snapshot.get("url").and_then(|v| v.as_str()) {
            if !url.is_empty() {
                let title = snapshot
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = snapshot
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let screenshot_path = snapshot
                    .get("screenshot_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                return Some(BrowsePreview {
                    url: url.to_string(),
                    title,
                    description,
                    screenshot_path,
                });
            }
        }
    }

    // Fall back to Display-formatted text parsing:
    // # Page Title
    // URL: https://...
    let mut title = String::new();
    let mut url = String::new();
    for line in output.lines() {
        if let Some(t) = line.strip_prefix("# ") {
            title = t.trim().to_string();
        } else if let Some(u) = line.strip_prefix("URL: ") {
            url = u.trim().to_string();
        }
    }
    if url.is_empty() {
        return None;
    }
    Some(BrowsePreview {
        url,
        title,
        description: None,
        screenshot_path: None,
    })
}

/// Render a website preview card styled like a link unfurl (Slack/Discord style).
///
/// Shows a left accent bar, domain, title, description, and "Open in Browser" action.
fn render_website_preview_card(
    index: usize,
    preview: &BrowsePreview,
    link_url: String,
    cx: &App,
) -> impl IntoElement {
    let text_color = cx.theme().foreground;
    let muted_text = cx.theme().muted_foreground;
    let panel_bg = cx.theme().muted;
    let accent = cx.theme().accent;

    let domain = extract_domain(&preview.url);
    let link_url_for_click = link_url.clone();

    div()
        .id(ElementId::Name(
            format!("browse-preview-{}", index).into(),
        ))
        .mt_1()
        .flex()
        .flex_row()
        // Left accent bar (link unfurl style)
        .child(
            div()
                .w(px(3.0))
                .flex_shrink_0()
                .bg(accent)
                .rounded_l_sm(),
        )
        // Content area
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .bg(panel_bg)
                .rounded_r_md()
                .flex()
                .flex_col()
                .gap(px(4.0))
                // Domain row with Earth icon
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Icon::new(CustomIcon::Earth)
                                .size(px(12.0))
                                .text_color(muted_text),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted_text)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(domain),
                        ),
                )
                // Title
                .when(!preview.title.is_empty(), |this| {
                    this.child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(text_color)
                            .overflow_hidden()
                            .child(preview.title.clone()),
                    )
                })
                // Description (if available)
                .when_some(preview.description.clone(), |this, desc| {
                    if desc.is_empty() {
                        this
                    } else {
                        let truncated = if desc.chars().count() > 200 {
                            let t: String = desc.chars().take(199).collect();
                            format!("{}…", t)
                        } else {
                            desc
                        };
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted_text)
                                .line_height(rems(1.4))
                                .child(truncated),
                        )
                    }
                })
                // Screenshot thumbnail (if available)
                .when_some(preview.screenshot_path.clone(), |this, path| {
                    let screenshot_file = std::path::PathBuf::from(&path);
                    if screenshot_file.exists() {
                        this.child(
                            div()
                                .mt(px(2.0))
                                .w_full()
                                .rounded_sm()
                                .overflow_hidden()
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(
                                    img(screenshot_file)
                                        .w_full()
                                        .max_h(px(280.0))
                                        .object_fit(gpui::ObjectFit::Contain),
                                ),
                        )
                    } else {
                        this
                    }
                })
                // "Open in Browser" action row
                .child(
                    div().mt(px(2.0)).child(
                        Button::new(ElementId::Name(
                            format!("open-browse-{}", index).into(),
                        ))
                        .label("Open in Browser")
                        .icon(Icon::new(CustomIcon::ExternalLink))
                        .xsmall()
                        .ghost()
                        .on_click(move |_, _, _cx| {
                            let url = if link_url_for_click.starts_with("http://")
                                || link_url_for_click.starts_with("https://")
                            {
                                link_url_for_click.clone()
                            } else {
                                format!("https://{}", link_url_for_click)
                            };
                            if let Err(e) = open::that_detached(&url) {
                                tracing::warn!(url = %url, error = %e, "Failed to open URL in browser");
                            }
                        }),
                    ),
                ),
        )
}
