//! `render_tool_call_inline` — renders a single tool call as a card in
//! the assistant message body (used by `message_component`).
//!
//! # What lives here
//!
//! - The public `render_tool_call_inline` entry point.
//! - Internal helpers for diff rendering, command extraction, output
//!   formatting, and a small `SelectableText` element wrapper.
//!
//! This is the "frozen" rendering path used after a stream has
//! completed (history view), whereas `blocks.rs` renders the live
//! collapsible trace while a stream is in progress.

#![allow(clippy::collapsible_if)]

use gpui::{prelude::FluentBuilder, *};
use gpui_component::{ActiveTheme, text::TextView};
use std::time::Duration;

use super::super::code_block_component::CodeBlockComponent;
use super::super::diff_view_component::DiffViewComponent;
use super::super::message_types::{ToolCallBlock, ToolCallState};
use super::badges::{
    is_code_execution_tool, render_execution_mode_badge, render_sub_agent_mode_badge,
};

pub struct InlineToolCallRenderArgs<'a, F, D>
where
    F: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    D: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
{
    pub tool_call: &'a ToolCallBlock,
    pub message_index: usize,
    pub tool_index: usize,
    pub collapsed: bool,
    pub on_toggle: F,
    pub diff_expanded: bool,
    pub on_expand_diff: D,
}

pub fn render_tool_call_inline<'a, F, D>(
    args: InlineToolCallRenderArgs<'a, F, D>,
    cx: &'a App,
) -> impl IntoElement + 'a
where
    F: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    D: Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
{
    let InlineToolCallRenderArgs {
        tool_call,
        message_index,
        tool_index,
        collapsed,
        on_toggle,
        diff_expanded,
        on_expand_diff,
    } = args;
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

    // Show runnable code with syntax highlighting when available.
    if let Some(code_block) = render_code_run_input(tool_call, message_index * 1000 + tool_index) {
        content_children.push(code_block);
    } else {
        let full_command = extract_full_command(tool_call);
        if full_command.chars().count() > 80 {
            content_children.push(render_full_command_box(full_command, panel_bg, text_color));
        }
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
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .bg(panel_bg.opacity(0.3))
        .child(header)
        .when(!collapsed && !content_children.is_empty(), |this| {
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
pub(super) fn try_build_diff_view(
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
pub(super) fn render_full_command_box(
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

pub(super) fn render_code_run_input(
    tool_call: &ToolCallBlock,
    block_index: usize,
) -> Option<AnyElement> {
    let (language, code) = extract_code_run_input(tool_call)?;
    Some(CodeBlockComponent::new(Some(language), code, block_index).into_any_element())
}

/// Format the inline header text for a tool call.
///
/// Most tools show `$ <command>` (shell-style), but internet and memory tools use their
/// friendly name as a prefix (e.g. "Searching online: rust async patterns").
pub(super) fn format_tool_call_header(tool_call: &ToolCallBlock) -> String {
    let detail = extract_command_display(tool_call);

    match tool_call.tool_name.as_str() {
        "sub_agent" => format!("{}: {}", tool_call.display_name, detail),
        "remember" | "search_memory" | "search_web" | "fetch" | "daytona_run" | "browser_use" => {
            // Use the friendly display_name as prefix with the detail
            format!("{}: {}", tool_call.display_name, detail)
        }
        _ => format!("$ {}", detail),
    }
}

/// Extract a user-friendly display string from tool call input (truncated for headers)
pub(super) fn extract_command_display(tool_call: &ToolCallBlock) -> String {
    let full = extract_full_command(tool_call);
    if full.chars().count() > 80 {
        let truncated: String = full.chars().take(77).collect();
        format!("{}...", truncated)
    } else {
        full
    }
}

/// Extract the full, untruncated command string from tool call input
pub(super) fn extract_full_command(tool_call: &ToolCallBlock) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&tool_call.input) {
        // For shell_execute tool: extract "command" field
        if tool_call.tool_name == "shell_execute" {
            if let Some(command) = json.get("command").and_then(|v| v.as_str()) {
                return command.to_string();
            }
        }

        // For execute_code / daytona_run: show language prefix + full code
        if tool_call.tool_name == "execute_code" || tool_call.tool_name == "daytona_run" {
            let language = json
                .get("language")
                .and_then(|v| v.as_str())
                .unwrap_or("python");
            let code = json.get("code").and_then(|v| v.as_str()).unwrap_or("");
            return format!("[{}] {}", language, code);
        }

        // For browser_use: show the task description
        if tool_call.tool_name == "browser_use" {
            if let Some(task) = json.get("task").and_then(|v| v.as_str()) {
                return task.to_string();
            }
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

pub(super) fn extract_code_run_input(tool_call: &ToolCallBlock) -> Option<(String, String)> {
    if !matches!(tool_call.tool_name.as_str(), "execute_code" | "daytona_run") {
        return None;
    }

    let json = serde_json::from_str::<serde_json::Value>(&tool_call.input).ok()?;
    let language = json
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("python")
        .to_string();
    let code = json.get("code").and_then(|v| v.as_str())?.to_string();
    Some((language, code))
}

/// Format tool call output for display (extract useful info from JSON)
pub(super) fn format_tool_output(output: &str) -> String {
    // Try to parse as JSON
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        // If it's an object with common result fields, extract them
        if let Some(obj) = json.as_object() {
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

            // daytona_run output: show result + exit_code + downloaded files
            if obj.contains_key("exit_code") && obj.contains_key("sandbox_cleaned_up") {
                let mut parts: Vec<String> = Vec::new();

                if let Some(result) = obj.get("result").and_then(|v| v.as_str()) {
                    if !result.is_empty() {
                        parts.push(result.to_string());
                    }
                }
                if let Some(code) = obj.get("exit_code").and_then(|v| v.as_i64()) {
                    if code != 0 {
                        parts.push(format!("[exit code: {}]", code));
                    }
                }
                if let Some(files) = obj.get("downloaded_files").and_then(|v| v.as_array()) {
                    if !files.is_empty() {
                        let names: Vec<&str> = files
                            .iter()
                            .filter_map(|v| v.as_str())
                            .filter_map(|p| p.rsplit('/').next())
                            .collect();
                        parts.push(format!("[files: {}]", names.join(", ")));
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
pub(super) struct SelectableText {
    id: ElementId,
    text: SharedString,
}

impl SelectableText {
    pub(super) fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
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
pub(super) fn escape_markdown(s: &str) -> SharedString {
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
