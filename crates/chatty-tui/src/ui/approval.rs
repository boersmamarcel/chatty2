use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::engine::ChatEngine;
use crate::ui::theme;

pub fn render_approval_prompt(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let approval = match &engine.pending_approval {
        Some(a) => a,
        None => return,
    };

    let sandboxed_indicator = if approval.is_sandboxed {
        Span::styled(" [sandboxed] ", theme::success())
    } else {
        Span::styled(" [host] ", theme::error())
    };

    let command_display = if approval.command.len() > 60 {
        format!("{}...", &approval.command[..60])
    } else {
        approval.command.clone()
    };

    let mut spans = vec![
        Span::styled(
            " APPROVE? ",
            Style::default()
                .fg(Color::Black)
                .bg(theme::WARNING)
                .add_modifier(Modifier::BOLD),
        ),
        sandboxed_indicator,
        Span::raw(command_display),
    ];
    // Relayed from a delegated agent (AGE-751): who asked, and its chain,
    // outside the command — never folded into the string above.
    if let Some(asker) = &approval.asker {
        spans.push(Span::styled(
            format!(
                " (asked by {} via {}) ",
                asker.agent,
                asker.chain.join(" \u{203a} ")
            ),
            theme::muted(),
        ));
    }
    spans.extend([
        Span::styled("  ", Style::default()),
        Span::styled("[y]", theme::success().add_modifier(Modifier::BOLD)),
        Span::raw("es / "),
        Span::styled("[n]", theme::error().add_modifier(Modifier::BOLD)),
        Span::raw("o"),
    ]);
    let line = Line::from(spans);

    let paragraph = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border()),
    );

    frame.render_widget(paragraph, area);
}
