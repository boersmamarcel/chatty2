//! Dim one-line footer that hints at the most useful shortcuts.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::engine::ChatEngine;
use crate::ui::theme;

pub fn render_hint_bar(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let mut left = vec![
        Span::styled("/", theme::accent()),
        Span::styled(" commands  ", theme::muted()),
        Span::styled("@", theme::success()),
        Span::styled(" files  ", theme::muted()),
        Span::styled("↑↓ / wheel", theme::accent()),
        Span::styled(" scroll  ", theme::muted()),
        Span::styled("Shift+drag", theme::accent()),
        Span::styled(" to select", theme::muted()),
    ];

    let right_text = if engine.is_streaming {
        "Ctrl+C stop  ·  Ctrl+Q quit"
    } else {
        "Ctrl+Q quit"
    };
    let right = Span::styled(right_text, theme::muted());

    // Best-effort right alignment using a padding span.
    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len = right.content.chars().count();
    if (left_len + right_len + 1) < area.width as usize {
        let pad = area.width as usize - left_len - right_len;
        left.push(Span::raw(" ".repeat(pad)));
        left.push(right);
    }

    let paragraph = Paragraph::new(Line::from(left));
    frame.render_widget(paragraph, area);
}
