use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::engine::ModelPicker;

pub fn render_model_picker(frame: &mut Frame, area: Rect, picker: &ModelPicker) {
    // Center the popup: 50 chars wide, height = items + 4 (borders + help line)
    let popup_width = 50u16.min(area.width.saturating_sub(4));
    let popup_height = (picker.items.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup_area = centered_rect(popup_width, popup_height, area);

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Select Model (↑↓ Enter Esc) ")
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Split inner into list area and help line
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    // Build list items
    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|item| {
            let active_marker = if item.is_active { " (active)" } else { "" };
            let line = Line::from(vec![
                Span::styled(&item.name, Style::default().fg(Color::White)),
                Span::styled(
                    format!(" — {}{}", item.provider, active_marker),
                    Style::default().fg(Color::DarkGray),
                ),
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

    // Help line
    let help = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Cyan)),
        Span::raw(" navigate  "),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::raw(" select  "),
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
