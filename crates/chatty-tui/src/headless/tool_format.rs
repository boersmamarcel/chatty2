//! Tool-call formatting helpers for headless / pipe output.
//!
//! These are pure formatting functions that turn a `ToolCallInfo` into the
//! line-oriented output the CLI prints to stdout. No I/O, no async, no
//! engine state mutation.

use super::*;
use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use crate::engine::{ToolCallInfo, ToolCallState};

pub(super) fn format_tool_call_lines(tc: &ToolCallInfo) -> Vec<String> {
    let mut lines = vec![format_tool_call_header(tc)];

    append_tool_payload(&mut lines, "input", &tc.input);

    if let Some(output) = &tc.output {
        let label = match tc.state {
            ToolCallState::Error => "error",
            _ => "output",
        };
        append_tool_payload(&mut lines, label, output);
    }

    lines
}

pub(super) fn format_tool_call_header(tc: &ToolCallInfo) -> String {
    let (icon, status) = match tc.state {
        ToolCallState::Running => ("\u{27f3}", "running"),
        ToolCallState::Success => ("\u{2713}", "completed"),
        ToolCallState::Error => ("\u{2717}", "failed"),
    };

    let mut header = format!("  [tool: {}] ", tc.name);
    if let Some(engine) = tc.execution_engine {
        header.push_str(&format!("[{}] ", engine_location_label(engine)));
    } else if let Some(source) = source_badge_label(&tc.source) {
        header.push_str(&format!("[{}] ", source));
    }
    header.push_str(&format!("{icon} {status}"));
    header
}

pub(super) fn append_tool_payload(lines: &mut Vec<String>, label: &str, content: &str) {
    let payload_lines = tool_payload_lines(content);
    if payload_lines.is_empty() {
        return;
    }

    lines.push(format!("    {label}"));
    for payload_line in payload_lines {
        lines.push(format!("      {payload_line}"));
    }
}

pub(super) fn tool_payload_lines(content: &str) -> Vec<String> {
    let content = content.trim_matches('\n');
    if content.trim().is_empty() {
        return Vec::new();
    }

    let pretty = serde_json::from_str::<serde_json::Value>(content.trim())
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok());
    let display = pretty.as_deref().unwrap_or(content);

    display.lines().map(str::to_string).collect()
}

pub(super) fn source_badge_label(source: &ToolSource) -> Option<&'static str> {
    match source {
        ToolSource::Local => None,
        ToolSource::HiveCloud => Some("remote"),
        ToolSource::Internet { .. } => Some("remote"),
        ToolSource::ExternalService { .. } => Some("remote"),
    }
}

pub(super) fn engine_location_label(engine: ExecutionEngine) -> &'static str {
    match engine {
        ExecutionEngine::Shell => "shell (local)",
        ExecutionEngine::Monty => "monty (local)",
        ExecutionEngine::Docker => "docker (local)",
        ExecutionEngine::Daytona => "daytona (remote)",
    }
}
