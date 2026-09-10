use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

use crate::engine::{
    ChatEngine, DisplayMessage, MessageBlock, MessageRole, ToolCallInfo, ToolCallState,
};
use crate::ui::theme;
use crate::ui::tool_summary;

pub fn render_messages(frame: &mut Frame, area: Rect, engine: &mut ChatEngine) {
    // Remember the chat area so mouse wheel events can route correctly.
    engine.last_chat_area = area;

    let mut lines: Vec<Line> = Vec::new();

    if !engine.is_ready {
        lines.push(Line::from(Span::styled("Initializing...", theme::muted())));
    } else if engine.transcript.messages.is_empty() {
        render_welcome_state(&mut lines, engine);
    } else {
        let verbose = engine.verbose_tools;
        for msg in &engine.transcript.messages {
            render_message(&mut lines, msg, verbose);
            lines.push(Line::from("")); // spacing between messages
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(format!(" {} ", engine.title));
    let inner = block.inner(area);
    let inner_width = inner.width;
    let visible_height = inner.height;

    // Calculate wrapped content height (accounts for word-wrap).
    let content_height = wrapped_line_count(&lines, inner_width);
    let max_scroll = content_height.saturating_sub(visible_height);

    // Autoscroll-pause: when unpinned and new content arrived, shift scroll_offset
    // by the growth so the user's visible window stays locked in place.
    if !engine.pinned_to_bottom && content_height > engine.last_content_height {
        let growth = content_height - engine.last_content_height;
        engine.scroll_offset = engine.scroll_offset.saturating_add(growth);
    }
    engine.last_content_height = content_height;

    // Clamp scroll_offset and decide final scroll position from the top.
    let scroll = if engine.pinned_to_bottom {
        engine.scroll_offset = 0;
        max_scroll
    } else {
        engine.scroll_offset = engine.scroll_offset.min(max_scroll);
        // Snap back to pinned when user scrolled all the way down.
        if engine.scroll_offset == 0 {
            engine.pinned_to_bottom = true;
        }
        max_scroll.saturating_sub(engine.scroll_offset)
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);

    // Scrollbar on the right edge when content overflows.
    if max_scroll > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .thumb_style(theme::accent())
            .track_style(theme::muted());
        let mut state = ScrollbarState::new(max_scroll as usize).position(scroll as usize);
        frame.render_stateful_widget(scrollbar, area, &mut state);
    }
}

fn render_welcome_state(lines: &mut Vec<Line>, engine: &ChatEngine) {
    let logo_style = theme::accent().add_modifier(Modifier::BOLD);
    for line in [
        " ██████╗██╗  ██╗ █████╗ ████████╗████████╗██╗   ██╗",
        "██╔════╝██║  ██║██╔══██╗╚══██╔══╝╚══██╔══╝╚██╗ ██╔╝",
        "██║     ███████║███████║   ██║      ██║    ╚████╔╝ ",
        "██║     ██╔══██║██╔══██║   ██║      ██║     ╚██╔╝  ",
        "╚██████╗██║  ██║██║  ██║   ██║      ██║      ██║   ",
        " ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝      ╚═╝      ╚═╝   ",
    ] {
        lines.push(Line::from(Span::styled(line, logo_style)));
    }

    let search_enabled = engine
        .search_settings
        .as_ref()
        .is_some_and(|settings| settings.enabled);
    let search_label = engine
        .search_settings
        .as_ref()
        .filter(|settings| settings.enabled)
        .map(|settings| format!("search {}", settings.active_provider))
        .unwrap_or_else(|| "search".to_string());
    let browser_use_enabled = engine.search_settings.as_ref().is_some_and(|settings| {
        settings.browser_use_enabled
            && settings
                .browser_use_api_key
                .as_ref()
                .is_some_and(|key| !key.trim().is_empty())
    });
    let daytona_enabled = engine.search_settings.as_ref().is_some_and(|settings| {
        settings.daytona_enabled
            && settings
                .daytona_api_key
                .as_ref()
                .is_some_and(|key| !key.trim().is_empty())
    });
    let remote_agent_count = engine
        .remote_agents
        .iter()
        .filter(|agent| agent.enabled)
        .count();
    let local_module_count = engine
        .module_agents
        .iter()
        .filter(|agent| !matches!(agent.execution_mode.as_str(), "remote" | "remote_only"))
        .count();
    let remote_module_count = engine
        .module_agents
        .iter()
        .filter(|agent| matches!(agent.execution_mode.as_str(), "remote" | "remote_only"))
        .count();

    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Terminal AI chat for developers",
            theme::muted(),
        )),
        Line::from(Span::styled(
            "Chatty can switch models, reshape tool access live, delegate work, and mix local + remote capabilities in one session.",
            theme::muted(),
        )),
        Line::from(""),
        welcome_line(
            "Model",
            vec![
                Span::styled(engine.model_config.name.clone(), theme::text_bold()),
                Span::raw(" via "),
                Span::styled(
                    engine.model_config.provider_type.display_name().to_string(),
                    theme::accent(),
                ),
                Span::raw(" · "),
                Span::styled(model_context_label(engine), theme::muted()),
            ],
        ),
        welcome_line(
            "Workspace",
            vec![Span::styled(
                engine.current_working_directory(),
                theme::text(),
            )],
        ),
        welcome_line(
            "Git",
            match engine.git_branch.as_deref() {
                Some(branch) => vec![Span::styled(branch.to_string(), theme::tool_bold())],
                None => vec![Span::styled("Not a git workspace".to_string(), theme::muted())],
            },
        ),
        welcome_line(
            "Tools",
            join_spans(vec![
                badge("shell", engine.execution_settings.enabled),
                badge("fs-read", engine.execution_settings.filesystem_read_enabled),
                badge("fs-write", engine.execution_settings.filesystem_write_enabled),
                badge("git", engine.execution_settings.git_enabled),
                badge("code", engine.execution_settings.execute_code_enabled),
                badge("docker", engine.execution_settings.docker_code_execution_enabled),
            ]),
        ),
        welcome_line(
            "Internet",
            join_spans(vec![
                badge("fetch", engine.execution_settings.fetch_enabled),
                badge(search_label, search_enabled),
                badge("browser-use", browser_use_enabled),
                badge("daytona", daytona_enabled),
                if engine.services_loaded {
                    badge("MCP", engine.mcp_service.is_some())
                } else {
                    loading_badge("MCP")
                },
            ]),
        ),
        welcome_line(
            "Runtime",
            join_spans(vec![
                if engine.services_loaded {
                    badge("memory", engine.memory_service.is_some())
                } else {
                    loading_badge("memory")
                },
                if engine.services_loaded {
                    badge(
                        "semantic memory",
                        engine.memory_service.is_some() && engine.embedding_service.is_some(),
                    )
                } else {
                    loading_badge("semantic memory")
                },
                badge("modules", engine.module_settings.enabled),
                badge(format!("module local {local_module_count}"), local_module_count > 0),
                badge(
                    format!("module remote {remote_module_count}"),
                    remote_module_count > 0,
                ),
                badge("local agent", !engine.is_sub_agent),
                badge(format!("remote {remote_agent_count}"), remote_agent_count > 0),
            ]),
        ),
        Line::from(""),
        welcome_line(
            "Try",
            vec![
                command_span("/tools"),
                Span::raw(" "),
                command_span("/model"),
                Span::raw(" "),
                command_span("/modules"),
                Span::raw(" "),
                command_span("/agent"),
                Span::raw(" "),
                command_span("/add-dir"),
                Span::raw(" "),
                command_span("/context"),
                Span::raw(" "),
                command_span("@file"),
            ],
        ),
        Line::from(Span::styled(
            "Send a message to start chatting.",
            theme::muted(),
        )),
    ]);
}

fn render_message(lines: &mut Vec<Line>, msg: &DisplayMessage, verbose: bool) {
    // Role label
    let (label, style) = match msg.role {
        MessageRole::User => ("you", theme::success_bold()),
        MessageRole::Assistant => ("assistant", theme::accent_bold()),
        MessageRole::System => ("system", theme::warning().add_modifier(Modifier::BOLD)),
    };

    lines.push(Line::from(Span::styled(format!("[{}]", label), style)));

    // Render blocks in the order they arrived so text and tool calls interleave.
    for block in &msg.blocks {
        match block {
            MessageBlock::Text(text) => {
                for line in text.lines() {
                    lines.push(Line::from(line.to_string()));
                }
                // Preserve a trailing empty line when the text ended on a newline.
                if text.ends_with('\n') {
                    lines.push(Line::from(""));
                }
            }
            MessageBlock::ToolCall(tc) => {
                render_tool_call(lines, tc, verbose);
            }
        }
    }

    // Streaming cursor — only when actively streaming text (no trailing tool call).
    let trailing_tool = matches!(msg.blocks.last(), Some(MessageBlock::ToolCall(_)));
    if msg.is_streaming && !trailing_tool {
        lines.push(Line::from(Span::styled("▌", theme::warning())));
    }
}

fn render_tool_call(lines: &mut Vec<Line>, tc: &ToolCallInfo, verbose: bool) {
    if verbose {
        render_tool_call_verbose(lines, tc);
    } else {
        render_tool_call_collapsed(lines, tc);
    }
}

/// One header line plus a folded result — what the user sees by default.
fn render_tool_call_collapsed(lines: &mut Vec<Line>, tc: &ToolCallInfo) {
    let (icon, tc_style) = tool_state_style(&tc.state);

    let mut header = vec![
        Span::raw("  "),
        Span::styled(icon, tc_style),
        Span::raw(" "),
        Span::styled(tc.name.clone(), theme::tool()),
        Span::styled(
            format!("({})", tool_summary::summarize_input(&tc.input)),
            theme::text_subtle(),
        ),
    ];
    if let Some(badge) = tool_badge_span(tc) {
        header.push(Span::raw(" "));
        header.push(badge);
    }
    match &tc.state {
        ToolCallState::Running => {
            header.push(Span::raw(" "));
            header.push(Span::styled("running", tc_style));
        }
        ToolCallState::Error => {
            header.push(Span::raw(" "));
            header.push(Span::styled("failed", tc_style));
        }
        ToolCallState::Success => {}
    }
    lines.push(Line::from(header));

    let Some(output) = tc.output.as_ref() else {
        return;
    };

    // A failure is the one payload worth reading in full — never fold it.
    if matches!(tc.state, ToolCallState::Error) {
        push_result_lines(lines, tool_summary::output_lines(output), 0, theme::error());
        return;
    }

    let (kept, hidden) = tool_summary::fold(tool_summary::output_lines(output));
    push_result_lines(lines, kept, hidden, theme::text_subtle());
}

/// Result body under the header: `⎿` on the first row, aligned after that,
/// closed by a `… +N lines` marker when anything was folded away.
fn push_result_lines(
    lines: &mut Vec<Line>,
    body: Vec<String>,
    hidden: usize,
    style: ratatui::style::Style,
) {
    for (index, line) in body.into_iter().enumerate() {
        let prefix = if index == 0 { "    ⎿ " } else { "      " };
        lines.push(Line::from(vec![
            Span::raw(prefix),
            Span::styled(line, style),
        ]));
    }
    if hidden > 0 {
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(format!("… +{hidden} lines"), theme::muted()),
        ]));
    }
}

/// Full input and output payloads, pretty-printed. Reached via `Ctrl+R`.
fn render_tool_call_verbose(lines: &mut Vec<Line>, tc: &ToolCallInfo) {
    let (icon, tc_style) = tool_state_style(&tc.state);
    let status = match &tc.state {
        ToolCallState::Running => "running",
        ToolCallState::Success => "completed",
        ToolCallState::Error => "failed",
    };

    let mut header = vec![Span::styled(
        format!("  [tool: {}] ", tc.name),
        theme::tool(),
    )];
    if let Some(badge) = tool_badge_span(tc) {
        header.push(badge);
        header.push(Span::raw(" "));
    }
    header.extend([
        Span::styled(icon, tc_style),
        Span::raw(" "),
        Span::styled(status, tc_style),
    ]);
    lines.push(Line::from(header));

    render_tool_payload(lines, "input", &tc.input, theme::text_subtle());

    if let Some(ref output) = tc.output {
        let (label, out_style) = match &tc.state {
            ToolCallState::Error => ("error", theme::error()),
            _ => ("output", theme::text_subtle()),
        };
        render_tool_payload(lines, label, output, out_style);
    }
}

fn tool_state_style(state: &ToolCallState) -> (&'static str, ratatui::style::Style) {
    match state {
        ToolCallState::Running => ("⟳", theme::warning()),
        ToolCallState::Success => ("✓", theme::success()),
        ToolCallState::Error => ("✗", theme::error()),
    }
}

fn render_tool_payload(
    lines: &mut Vec<Line>,
    label: &str,
    content: &str,
    content_style: ratatui::style::Style,
) {
    let payload_lines = tool_payload_lines(content);
    if payload_lines.is_empty() {
        return;
    }

    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(label.to_string(), theme::muted()),
    ]));

    for payload_line in payload_lines {
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(payload_line, content_style),
        ]));
    }
}

fn tool_payload_lines(content: &str) -> Vec<String> {
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

/// Where the call ran, as a badge — `None` when it ran locally with no engine
/// worth naming.
fn tool_badge_span(tc: &ToolCallInfo) -> Option<Span<'static>> {
    if let Some(engine) = tc.execution_engine {
        return Some(Span::styled(
            format!("[{}]", engine_location_label(engine)),
            theme::muted(),
        ));
    }
    match &tc.source {
        chatty_core::models::message_types::ToolSource::Local => None,
        chatty_core::models::message_types::ToolSource::HiveCloud => {
            Some(Span::styled("[remote]", theme::accent()))
        }
        chatty_core::models::message_types::ToolSource::Internet { .. } => {
            Some(Span::styled("[remote]", theme::warning()))
        }
        chatty_core::models::message_types::ToolSource::ExternalService { .. } => {
            Some(Span::styled("[remote]", theme::accent()))
        }
    }
}

fn engine_location_label(
    engine: chatty_core::models::message_types::ExecutionEngine,
) -> &'static str {
    match engine {
        chatty_core::models::message_types::ExecutionEngine::Shell => "shell (local)",
        chatty_core::models::message_types::ExecutionEngine::Monty => "monty (local)",
        chatty_core::models::message_types::ExecutionEngine::Docker => "docker (local)",
        chatty_core::models::message_types::ExecutionEngine::Daytona => "daytona (remote)",
    }
}

/// Estimate the number of visual rows after word-wrap.
fn wrapped_line_count(lines: &[Line], wrap_width: u16) -> u16 {
    if wrap_width == 0 {
        return lines.len() as u16;
    }
    let w = wrap_width as usize;
    lines
        .iter()
        .map(|line| {
            let line_width = line.width();
            if line_width <= w {
                1u16
            } else {
                line_width.div_ceil(w) as u16
            }
        })
        .sum()
}

fn welcome_line(label: &str, mut spans: Vec<Span<'static>>) -> Line<'static> {
    let mut line = vec![Span::styled(format!("{label:<10}"), theme::muted())];
    line.append(&mut spans);
    Line::from(line)
}

fn badge(label: impl Into<String>, enabled: bool) -> Span<'static> {
    let style = if enabled {
        theme::success_bold()
    } else {
        theme::muted()
    };
    Span::styled(format!("[{}]", label.into()), style)
}

fn loading_badge(label: impl Into<String>) -> Span<'static> {
    Span::styled(format!("[{} ⟳]", label.into()), theme::accent())
}

fn command_span(command: &str) -> Span<'static> {
    Span::styled(command.to_string(), theme::accent_bold())
}

fn join_spans(items: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut joined = Vec::with_capacity(items.len().saturating_mul(2));
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            joined.push(Span::raw(" "));
        }
        joined.push(item);
    }
    joined
}

fn model_context_label(engine: &ChatEngine) -> String {
    match engine.model_config.max_context_window {
        Some(max_context) if max_context > 0 => {
            format!("{} context", format_count(max_context as u32))
        }
        _ => "context unknown".to_string(),
    }
}

fn format_count(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.0}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::models::message_types::{ExecutionEngine, ToolSource};

    #[test]
    fn tool_payload_lines_pretty_print_json() {
        assert_eq!(
            tool_payload_lines(r#"{"command":"pwd","cwd":"/tmp"}"#),
            vec![
                "{".to_string(),
                r#"  "command": "pwd","#.to_string(),
                r#"  "cwd": "/tmp""#.to_string(),
                "}".to_string(),
            ]
        );
    }

    #[test]
    fn tool_payload_lines_keep_plain_text_lines() {
        assert_eq!(
            tool_payload_lines("stdout line 1\nstderr line 2\n"),
            vec!["stdout line 1".to_string(), "stderr line 2".to_string(),]
        );
    }

    fn tool_call(input: &str, output: Option<&str>, state: ToolCallState) -> ToolCallInfo {
        ToolCallInfo {
            id: "call-1".to_string(),
            name: "shell_execute".to_string(),
            input: input.to_string(),
            output: output.map(str::to_string),
            state,
            source: ToolSource::Local,
            execution_engine: Some(ExecutionEngine::Shell),
        }
    }

    fn render(tc: &ToolCallInfo, verbose: bool) -> Vec<String> {
        let mut lines = Vec::new();
        render_tool_call(&mut lines, tc, verbose);
        lines.iter().map(line_text).collect()
    }

    #[test]
    fn verbose_mode_shows_input_and_error_blocks() {
        let tc = tool_call(
            r#"{"command":"pwd"}"#,
            Some("Failed to spawn shell process"),
            ToolCallState::Error,
        );

        assert_eq!(
            render(&tc, true),
            vec![
                "  [tool: shell_execute] [shell (local)] ✗ failed".to_string(),
                "    input".to_string(),
                "      {".to_string(),
                r#"        "command": "pwd""#.to_string(),
                "      }".to_string(),
                "    error".to_string(),
                "      Failed to spawn shell process".to_string(),
            ]
        );
    }

    #[test]
    fn collapsed_mode_folds_a_long_result_behind_one_header() {
        let stdout = (1..=9)
            .map(|n| format!("DIR entry-{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tc = tool_call(
            r#"{"command":"ls -la /notes","cwd":"/notes"}"#,
            Some(&stdout),
            ToolCallState::Success,
        );

        assert_eq!(
            render(&tc, false),
            vec![
                "  ✓ shell_execute(ls -la /notes) [shell (local)]".to_string(),
                "    ⎿ DIR entry-1".to_string(),
                "      DIR entry-2".to_string(),
                "      DIR entry-3".to_string(),
                "      … +6 lines".to_string(),
            ]
        );
    }

    #[test]
    fn collapsed_mode_never_folds_an_error() {
        let tc = tool_call(
            r#"{"command":"pwd"}"#,
            Some("line 1\nline 2\nline 3\nline 4\nline 5"),
            ToolCallState::Error,
        );

        assert_eq!(
            render(&tc, false),
            vec![
                "  ✗ shell_execute(pwd) [shell (local)] failed".to_string(),
                "    ⎿ line 1".to_string(),
                "      line 2".to_string(),
                "      line 3".to_string(),
                "      line 4".to_string(),
                "      line 5".to_string(),
            ]
        );
    }

    #[test]
    fn collapsed_mode_shows_a_running_call_without_a_result() {
        let tc = tool_call(r#"{"command":"cargo test"}"#, None, ToolCallState::Running);

        assert_eq!(
            render(&tc, false),
            vec!["  ⟳ shell_execute(cargo test) [shell (local)] running".to_string()]
        );
    }

    #[test]
    fn collapsed_mode_drops_the_badge_separator_when_there_is_no_badge() {
        let mut tc = tool_call(r#"{"id":"t1"}"#, None, ToolCallState::Success);
        tc.name = "update_todo".to_string();
        tc.execution_engine = None;

        assert_eq!(render(&tc, false), vec!["  ✓ update_todo(t1)".to_string()]);
    }

    /// The screenshot in AGE-340: two calls that used to fill the viewport.
    #[test]
    fn collapsed_mode_keeps_the_screenshot_pair_under_eight_lines() {
        let shell = tool_call(
            r#"{"command":"ls /notes/KPMG"}"#,
            Some(
                r#"{"stdout":"DIR .obsidian\nDIR 00-meta\nDIR 01-maps\nDIR 02-domains\nDIR 03-projects\nDIR 04-decisions","exit_code":0,"truncated":false}"#,
            ),
            ToolCallState::Success,
        );
        let mut todo = tool_call(
            r#"{"id":"t1","status":"done"}"#,
            Some(
                r#"{"message":"Todo marked done.","snapshot":{"goal":"Set up the repository","todos":[{"id":"t1"},{"id":"t2"},{"id":"t3"}]}}"#,
            ),
            ToolCallState::Success,
        );
        todo.name = "update_todo".to_string();
        todo.execution_engine = None;

        let mut lines = Vec::new();
        render_tool_call(&mut lines, &shell, false);
        render_tool_call(&mut lines, &todo, false);

        assert!(lines.len() <= 8, "rendered {} lines", lines.len());
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }
}
