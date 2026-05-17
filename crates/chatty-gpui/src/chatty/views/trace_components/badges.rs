//! Small visual badge helpers used throughout the trace components.
//!
//! These are pure-rendering helpers — no state, no events, no `cx`
//! mutation — so isolating them keeps the block renderers in `blocks.rs`
//! focused on layout decisions instead of badge styling minutiae.

use gpui::*;

use super::super::message_types::{ExecutionEngine, ToolCallBlock, ToolSource};

pub(super) fn render_outline_badge(text: String, color: Rgba) -> AnyElement {
    div()
        .text_xs()
        .px_2()
        .py(px(0.5))
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_color(color)
        .child(text)
        .into_any_element()
}

pub(super) fn tool_source_badge(source: &ToolSource) -> Option<(String, Rgba)> {
    match source {
        ToolSource::HiveCloud => Some(("☁ Remote".to_string(), rgba(0x3B82F6ff))),
        ToolSource::Internet { label } => Some((format!("↗ {label}"), rgba(0xF59E0Bff))),
        ToolSource::ExternalService { name } => Some((format!("↗ {name}"), rgba(0xA855F7ff))),
        ToolSource::Local => None,
    }
}

pub(super) fn execution_engine_badge(engine: ExecutionEngine) -> (String, Rgba) {
    let color = match engine {
        ExecutionEngine::Shell => rgba(0x6B7280ff),
        ExecutionEngine::Monty => rgba(0x0EA5A4ff),
        ExecutionEngine::Docker => rgba(0x2563EBff),
        ExecutionEngine::Daytona => rgba(0x7C3AEDff),
    };
    let label = match engine {
        ExecutionEngine::Shell => "shell (local)",
        ExecutionEngine::Monty => "monty",
        ExecutionEngine::Docker => "docker",
        ExecutionEngine::Daytona => "daytona",
    };
    (label.to_string(), color)
}

pub(super) fn render_mode_badge(label: &'static str, is_remote: bool, badge_text: Hsla) -> AnyElement {
    let bg = if is_remote {
        rgba(0x3B82F6ff)
    } else {
        rgba(0x6B7280ff)
    };

    div()
        .text_xs()
        .px_2()
        .py(px(0.5))
        .rounded_sm()
        .bg(bg)
        .text_color(badge_text)
        .flex_shrink_0()
        .child(label)
        .into_any_element()
}

pub(super) fn sub_agent_mode_label(source: &ToolSource) -> &'static str {
    match source {
        ToolSource::HiveCloud
        | ToolSource::ExternalService { .. }
        | ToolSource::Internet { .. } => "remote",
        ToolSource::Local => "local",
    }
}

pub(super) fn render_sub_agent_mode_badge(source: &ToolSource, badge_text: Hsla) -> AnyElement {
    render_mode_badge(
        sub_agent_mode_label(source),
        !matches!(source, ToolSource::Local),
        badge_text,
    )
}

pub(super) fn is_code_execution_tool(tool_call: &ToolCallBlock) -> bool {
    matches!(tool_call.tool_name.as_str(), "execute_code" | "daytona_run")
}

pub(super) fn render_execution_mode_badge(engine: ExecutionEngine, badge_text: Hsla) -> AnyElement {
    let (label, color) = execution_engine_badge(engine);

    div()
        .text_xs()
        .px_2()
        .py(px(0.5))
        .rounded_sm()
        .bg(color)
        .text_color(badge_text)
        .flex_shrink_0()
        .child(label)
        .into_any_element()
}

