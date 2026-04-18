use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::APP_VERSION;
use crate::engine::ChatEngine;

pub fn render_status_bar(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let model_name = &engine.model_config.name;
    let cwd_max_len =
        usize::from(
            area.width
                .saturating_sub(if engine.git_branch.is_some() { 40 } else { 24 }),
        )
        .clamp(12, 48);
    let working_dir = truncate_middle(&engine.current_working_directory(), cwd_max_len);

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
        Span::styled("cwd ", Style::default().fg(Color::Gray)),
        Span::styled(working_dir, Style::default().fg(Color::White)),
    ];

    if let Some(branch) = engine.git_branch.as_deref() {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled("git ", Style::default().fg(Color::Gray)));
        spans.push(Span::styled(
            truncate_middle(branch, 18),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));

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
    if left_len + help_len < area.width {
        let padding = area.width.saturating_sub(left_len + help_len);
        spans.push(Span::raw(" ".repeat(padding as usize)));
        spans.push(Span::styled(
            help_text,
            Style::default().fg(Color::DarkGray),
        ));
    }

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

fn truncate_middle(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len || max_len <= 3 {
        return value.to_string();
    }

    let keep = (max_len - 1) / 2;
    let prefix: String = value.chars().take(keep).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(max_len - keep - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}
