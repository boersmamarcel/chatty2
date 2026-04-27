use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::ui::InputState;
use crate::ui::input::SlashMenuItem;
use crate::ui::theme;

pub fn render_slash_menu(frame: &mut Frame, area: Rect, input_state: &mut InputState) {
    let items = input_state.slash_menu_items();
    if items.is_empty() {
        input_state.set_slash_menu_scroll_offset(0);
        return;
    }

    let popup_width = 96u16.min(area.width.saturating_sub(4));
    let popup_height = (items.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup_area = centered_rect(popup_width, popup_height, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(" Slash Commands & Skills (↑↓ Tab/Enter) ")
        .style(Style::default().bg(ratatui::style::Color::Black));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    let list_items: Vec<ListItem> = items.iter().map(menu_item_to_list_item).collect();

    let list = List::new(list_items)
        .highlight_style(theme::highlight_accent())
        .highlight_symbol("▸ ");

    let selected = input_state
        .slash_menu_selected_index()
        .min(items.len().saturating_sub(1));
    let mut state = ListState::default()
        .with_offset(input_state.slash_menu_scroll_offset())
        .with_selected(Some(selected));
    frame.render_stateful_widget(list, chunks[0], &mut state);
    input_state.set_slash_menu_scroll_offset(state.offset());

    let help = Line::from(vec![
        Span::styled("Type /", theme::accent()),
        Span::raw(" to filter  "),
        Span::styled("↑↓", theme::accent()),
        Span::raw(" select  "),
        Span::styled("Tab/Enter", theme::accent()),
        Span::raw(" apply"),
    ]);
    frame.render_widget(Paragraph::new(help).style(theme::muted()), chunks[1]);
}

/// Convert a single slash-menu item into a ratatui `ListItem`, with distinct
/// styling for built-in commands (accent) vs. filesystem skills (tool color).
fn menu_item_to_list_item(item: &SlashMenuItem) -> ListItem<'static> {
    let display = item.display_command();
    let description = item.description().to_string();

    if item.is_skill() {
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:<18}", display), theme::tool()),
            Span::styled("[skill] ", theme::tool()),
            Span::styled(description, theme::text()),
        ]))
    } else {
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:<18}", display), theme::accent()),
            Span::styled(description, theme::text()),
        ]))
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
