use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::APP_VERSION;
use crate::engine::ChatEngine;

pub fn render_status_bar(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let model_name = &engine.model_config.name;

    let mut spans = vec![
        Span::styled(
            format!(" {} ", model_name),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("v{}", APP_VERSION),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
    ];

    // Token count
    if engine.total_input_tokens > 0 || engine.total_output_tokens > 0 {
        spans.push(Span::styled(
            format!(
                "{}↑ {}↓",
                format_tokens(engine.total_input_tokens),
                format_tokens(engine.total_output_tokens),
            ),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
    }

    // Status indicator
    if !engine.is_ready {
        spans.push(Span::styled(
            "● initializing…",
            Style::default().fg(Color::Cyan),
        ));
    } else if engine.is_streaming {
        spans.push(Span::styled(
            "● streaming",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled("● ready", Style::default().fg(Color::Green)));
    }

    // Right-aligned help
    let help_text = " Ctrl+C: stop/quit │ Ctrl+Q: quit ";
    let help_len = help_text.len() as u16;
    let left_len: u16 = spans.iter().map(|s| s.content.len() as u16).sum();
    let padding = area.width.saturating_sub(left_len + help_len);

    spans.push(Span::raw(" ".repeat(padding as usize)));
    spans.push(Span::styled(
        help_text,
        Style::default().fg(Color::DarkGray),
    ));

    let status_line = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));

    frame.render_widget(status_line, area);
}

fn format_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}
