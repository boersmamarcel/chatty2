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
        lines.push(Line::from(Span::styled(
            "Send a message to start chatting.",
            Style::default().fg(Color::DarkGray),
        )));
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
