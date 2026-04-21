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

pub fn render_messages(frame: &mut Frame, area: Rect, engine: &mut ChatEngine) {
    // Remember the chat area so mouse wheel events can route correctly.
    engine.last_chat_area = area;

    let mut lines: Vec<Line> = Vec::new();

    if !engine.is_ready {
        lines.push(Line::from(Span::styled("Initializing...", theme::muted())));
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

fn render_message(lines: &mut Vec<Line>, msg: &DisplayMessage) {
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
                render_tool_call(lines, tc);
            }
        }
    }

    // Streaming cursor — only when actively streaming text (no trailing tool call).
    let trailing_tool = matches!(msg.blocks.last(), Some(MessageBlock::ToolCall(_)));
    if msg.is_streaming && !trailing_tool {
        lines.push(Line::from(Span::styled("▌", theme::warning())));
    }
}

fn render_tool_call(lines: &mut Vec<Line>, tc: &ToolCallInfo) {
    let (icon, tc_style) = match &tc.state {
        ToolCallState::Running => ("⟳", theme::warning()),
        ToolCallState::Success => ("✓", theme::success()),
        ToolCallState::Error => ("✗", theme::error()),
    };

    lines.push(Line::from(vec![
        Span::styled(format!("  [tool: {}] ", tc.name), theme::tool()),
        Span::styled(icon, tc_style),
        Span::raw(" "),
        Span::styled(truncate(&tc.input, 60), theme::muted()),
    ]));

    if let Some(ref output) = tc.output {
        let preview = truncate(output, 80);
        let out_style = match &tc.state {
            ToolCallState::Error => theme::error(),
            _ => theme::muted(),
        };
        lines.push(Line::from(Span::styled(
            format!("    → {}", preview),
            out_style,
        )));
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
