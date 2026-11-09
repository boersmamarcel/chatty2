use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::engine::ChatEngine;
use crate::ui::theme;

/// The command as the one-line prompt shows it: at most 60 characters, then
/// `...` (AGE-1091). Cut by characters: a byte cut can land inside one and panic.
fn command_preview(command: &str) -> String {
    const MAX_CHARS: usize = 60;
    if command.chars().count() > MAX_CHARS {
        let head: String = command.chars().take(MAX_CHARS).collect();
        format!("{head}...")
    } else {
        command.to_string()
    }
}

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

    let command_display = command_preview(&approval.command);

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

#[cfg(test)]
mod tests {
    use super::command_preview;

    #[test]
    fn a_long_command_is_cut_by_characters() {
        let long = format!("x{}", "日".repeat(100));
        let shown = command_preview(&long);
        assert_eq!(shown.chars().count(), 63);
        assert!(shown.starts_with("x日"));
        assert!(shown.ends_with("..."));

        let mid = format!("x{}", "日".repeat(30));
        assert_eq!(command_preview(&mid), mid);
    }

    #[test]
    fn a_short_command_is_shown_whole() {
        assert_eq!(command_preview("ls -la"), "ls -la");
        assert_eq!(command_preview(&"a".repeat(61)), "a".repeat(60) + "...");
    }
}
