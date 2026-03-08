use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::engine::ChatEngine;

pub fn render_approval_prompt(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let approval = match &engine.pending_approval {
        Some(a) => a,
        None => return,
    };

    let sandboxed_indicator = if approval.is_sandboxed {
        Span::styled(" [sandboxed] ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" [host] ", Style::default().fg(Color::Red))
    };

    let command_display = if approval.command.len() > 60 {
        format!("{}...", &approval.command[..60])
    } else {
        approval.command.clone()
    };

    let line = Line::from(vec![
        Span::styled(
            " APPROVE? ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        sandboxed_indicator,
        Span::raw(command_display),
        Span::styled("  ", Style::default()),
        Span::styled(
            "[y]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("es / "),
        Span::styled(
            "[n]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("o"),
    ]);

    let paragraph = Paragraph::new(line).block(Block::default().borders(Borders::ALL));

    frame.render_widget(paragraph, area);
}
