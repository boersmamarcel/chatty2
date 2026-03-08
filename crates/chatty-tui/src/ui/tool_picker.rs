use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::engine::ToolPicker;

pub fn render_tool_picker(frame: &mut Frame, area: Rect, picker: &ToolPicker) {
    let popup_width = 55u16.min(area.width.saturating_sub(4));
    let popup_height = (picker.items.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup_area = centered_rect(popup_width, popup_height, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Tool Settings (↑↓ Space Enter Esc) ")
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|item| {
            let checkbox = if item.enabled { "[x]" } else { "[ ]" };
            let line = Line::from(vec![
                Span::styled(
                    format!("{} ", checkbox),
                    Style::default().fg(if item.enabled {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled(&item.label, Style::default().fg(Color::White)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let help = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Cyan)),
        Span::raw(" navigate  "),
        Span::styled("Space", Style::default().fg(Color::Cyan)),
        Span::raw(" toggle  "),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::raw(" apply  "),
        Span::styled("Esc", Style::default().fg(Color::Cyan)),
        Span::raw(" cancel"),
    ]);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
