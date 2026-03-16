use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::ui::InputState;

pub fn render_at_menu(frame: &mut Frame, area: Rect, input_state: &InputState) {
    let items = input_state.at_menu_items();
    if items.is_empty() {
        return;
    }

    let popup_width = 96u16.min(area.width.saturating_sub(4));
    let popup_height = (items.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup_area = centered_rect(popup_width, popup_height, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" @ File Mentions (↑↓ Tab/Enter) ")
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|name| {
            let line = Line::from(vec![Span::styled(
                name.to_string(),
                Style::default().fg(Color::Green),
            )]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(list_items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let selected = input_state
        .at_menu_selected_index()
        .min(items.len().saturating_sub(1));
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let help = Line::from(vec![
        Span::styled("Type @", Style::default().fg(Color::Green)),
        Span::raw(" to filter  "),
        Span::styled("↑↓", Style::default().fg(Color::Green)),
        Span::raw(" select  "),
        Span::styled("Tab/Enter", Style::default().fg(Color::Green)),
        Span::raw(" insert"),
    ]);
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
