use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::engine::{ChatEngine, DisplayMessage, MessageRole, ToolCallState};

pub fn render_messages(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let mut lines: Vec<Line> = Vec::new();

    if !engine.is_ready {
        lines.push(Line::from(Span::styled(
            "Initializing...",
            Style::default().fg(Color::DarkGray),
        )));
    } else if engine.messages.is_empty() {
        render_welcome_state(&mut lines, engine);
    } else {
        for msg in &engine.messages {
            render_message(&mut lines, msg);
            lines.push(Line::from("")); // spacing between messages
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", engine.title));
    let inner_width = block.inner(area).width;
    let visible_height = area.height.saturating_sub(2); // minus borders

    // Calculate wrapped content height (accounts for word-wrap)
    let content_height = wrapped_line_count(&lines, inner_width);
    let max_scroll = content_height.saturating_sub(visible_height);
    let scroll = max_scroll.saturating_sub(engine.scroll_offset);

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

fn render_welcome_state(lines: &mut Vec<Line>, engine: &ChatEngine) {
    let logo_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
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

    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Terminal AI chat for developers",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "Chatty can switch models, reshape tool access live, delegate work, and mix local + remote capabilities in one session.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        welcome_line(
            "Model",
            vec![
                Span::styled(
                    engine.model_config.name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" via "),
                Span::styled(
                    engine.model_config.provider_type.display_name().to_string(),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" · "),
                Span::styled(model_context_label(engine), Style::default().fg(Color::DarkGray)),
            ],
        ),
        welcome_line(
            "Workspace",
            vec![Span::styled(
                engine.current_working_directory(),
                Style::default().fg(Color::White),
            )],
        ),
        welcome_line(
            "Git",
            match engine.git_branch.as_deref() {
                Some(branch) => vec![Span::styled(
                    branch.to_string(),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )],
                None => vec![Span::styled(
                    "Not a git workspace".to_string(),
                    Style::default().fg(Color::DarkGray),
                )],
            },
        ),
        welcome_line(
            "Tools",
            join_spans(vec![
                badge("shell", engine.execution_settings.enabled),
                badge("fs-read", engine.execution_settings.filesystem_read_enabled),
                badge("fs-write", engine.execution_settings.filesystem_write_enabled),
                badge("git", engine.execution_settings.git_enabled),
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
                badge("MCP", engine.mcp_service.is_some()),
            ]),
        ),
        welcome_line(
            "Runtime",
            join_spans(vec![
                badge("memory", engine.memory_service.is_some()),
                badge(
                    "semantic memory",
                    engine.memory_service.is_some() && engine.embedding_service.is_some(),
                ),
                badge("modules", engine.module_settings.enabled),
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
            Style::default().fg(Color::DarkGray),
        )),
    ]);
}

fn render_message(lines: &mut Vec<Line>, msg: &DisplayMessage) {
    // Role label
    let (label, color) = match msg.role {
        MessageRole::User => ("you", Color::Green),
        MessageRole::Assistant => ("assistant", Color::Cyan),
        MessageRole::System => ("system", Color::Yellow),
    };

    lines.push(Line::from(Span::styled(
        format!("[{}]", label),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));

    // Message text
    if !msg.text.is_empty() {
        for line in msg.text.lines() {
            lines.push(Line::from(line.to_string()));
        }
    }

    // Streaming cursor
    if msg.is_streaming && msg.tool_calls.is_empty() {
        lines.push(Line::from(Span::styled(
            "▌",
            Style::default().fg(Color::Yellow),
        )));
    }

    // Tool calls
    for tc in &msg.tool_calls {
        let (icon, tc_color) = match &tc.state {
            ToolCallState::Running => ("⟳", Color::Yellow),
            ToolCallState::Success => ("✓", Color::Green),
            ToolCallState::Error => ("✗", Color::Red),
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  [tool: {}] ", tc.name),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(icon, Style::default().fg(tc_color)),
            Span::raw(" "),
            Span::styled(
                truncate(&tc.input, 60),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        if let Some(ref output) = tc.output {
            let preview = truncate(output, 80);
            let out_color = match &tc.state {
                ToolCallState::Error => Color::Red,
                _ => Color::DarkGray,
            };
            lines.push(Line::from(Span::styled(
                format!("    → {}", preview),
                Style::default().fg(out_color),
            )));
        }
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

fn truncate(s: &str, max_len: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() > max_len {
        format!("{}...", &first_line[..max_len])
    } else {
        first_line.to_string()
    }
}

fn welcome_line(label: &str, mut spans: Vec<Span<'static>>) -> Line<'static> {
    let mut line = vec![Span::styled(
        format!("{label:<10}"),
        Style::default().fg(Color::DarkGray),
    )];
    line.append(&mut spans);
    Line::from(line)
}

fn badge(label: impl Into<String>, enabled: bool) -> Span<'static> {
    let style = if enabled {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Span::styled(format!("[{}]", label.into()), style)
}

fn command_span(command: &str) -> Span<'static> {
    Span::styled(
        command.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
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
