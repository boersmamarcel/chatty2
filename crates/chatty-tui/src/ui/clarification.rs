use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::engine::ChatEngine;
use crate::ui::theme;

/// Height the clarification prompt needs: header, question, options, hint.
pub const CLARIFICATION_HEIGHT: u16 = 6;

pub fn render_clarification_prompt(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let Some(pending) = &engine.pending_clarification else {
        return;
    };
    let Some(question) = pending.current_question() else {
        return;
    };

    let progress = if pending.questions.len() > 1 {
        format!(
            " QUESTION {}/{} ",
            pending.current + 1,
            pending.questions.len()
        )
    } else {
        " QUESTION ".to_string()
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                progress,
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::WARNING)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::raw(question.question.clone()),
        ]),
        Line::from(""),
    ];

    match &pending.custom {
        // Typing a free-text answer.
        Some(buffer) => {
            lines.push(Line::from(vec![
                Span::styled("> ", theme::success().add_modifier(Modifier::BOLD)),
                Span::raw(buffer.clone()),
                Span::styled("_", theme::success()),
            ]));
            lines.push(Line::from(Span::styled(
                "Enter to send · Esc to go back to the options",
                theme::border(),
            )));
        }
        // Picking from the pre-made options.
        None => {
            let mut spans: Vec<Span> = Vec::new();
            for (ix, option) in question.options.iter().enumerate() {
                if ix > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    format!("[{}]", ix + 1),
                    theme::success().add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::raw(format!(" {option}")));
            }
            lines.push(Line::from(spans));
            lines.push(Line::from(vec![
                Span::styled("[t]", theme::success().add_modifier(Modifier::BOLD)),
                Span::raw("ype your own answer"),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border()),
    );

    frame.render_widget(paragraph, area);
}
