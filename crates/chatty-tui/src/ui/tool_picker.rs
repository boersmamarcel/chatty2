use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::engine::ToolPicker;
use crate::ui::theme;

pub fn render_tool_picker(frame: &mut Frame, area: Rect, picker: &ToolPicker) {
    let popup_width = 55u16.min(area.width.saturating_sub(4));
    let popup_height = (picker.items.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup_area = centered_rect(popup_width, popup_height, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(" Tool Settings (↑↓ Space Enter Esc) ")
        .style(Style::default().bg(ratatui::style::Color::Black));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|item| {
            let checkbox = if item.enabled { "[x]" } else { "[ ]" };
            let check_style = if item.enabled {
                theme::success()
            } else {
                theme::muted()
            };
            let line = Line::from(vec![
                Span::styled(format!("{} ", checkbox), check_style),
                Span::styled(&item.label, theme::text()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(theme::highlight_accent())
        .highlight_symbol("▸ ");

    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let help = Line::from(vec![
        Span::styled("↑↓", theme::accent()),
        Span::raw(" navigate  "),
        Span::styled("Space", theme::accent()),
        Span::raw(" toggle  "),
        Span::styled("Enter", theme::accent()),
        Span::raw(" apply  "),
        Span::styled("Esc", theme::accent()),
        Span::raw(" cancel"),
    ]);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(help).style(theme::muted()),
        chunks[1],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
