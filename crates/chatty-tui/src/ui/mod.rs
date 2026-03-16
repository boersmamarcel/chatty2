mod approval;
mod at_menu;
mod chat_view;
mod input;
mod model_picker;
mod slash_menu;
mod status_bar;
mod tool_picker;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::engine::ChatEngine;

pub use input::InputState;

/// Render the full TUI layout
pub fn render(frame: &mut Frame, engine: &ChatEngine, input_state: &InputState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // Chat messages (fills remaining space)
            Constraint::Length(1), // Status bar
            Constraint::Length(3), // Input area
        ])
        .split(frame.area());

    // Chat messages area
    chat_view::render_messages(frame, chunks[0], engine);

    // Status bar
    status_bar::render_status_bar(frame, chunks[1], engine);

    // Input area (or approval prompt if pending)
    if engine.pending_approval.is_some() {
        approval::render_approval_prompt(frame, chunks[2], engine);
    } else {
        input::render_input(frame, chunks[2], input_state);
    }

    // Model picker overlay (rendered last so it appears on top)
    if let Some(ref picker) = engine.model_picker {
        model_picker::render_model_picker(frame, frame.area(), picker);
    }

    // Tool picker overlay
    if let Some(ref picker) = engine.tool_picker {
        tool_picker::render_tool_picker(frame, frame.area(), picker);
    }

    // Slash command menu overlay (only while typing a slash command)
    if engine.model_picker.is_none()
        && engine.tool_picker.is_none()
        && engine.pending_approval.is_none()
        && input_state.is_slash_menu_open()
    {
        slash_menu::render_slash_menu(frame, frame.area(), input_state);
    }

    // @ mention menu overlay (only while typing @<query>)
    if engine.model_picker.is_none()
        && engine.tool_picker.is_none()
        && engine.pending_approval.is_none()
        && !input_state.is_slash_menu_open()
        && input_state.is_at_menu_open()
    {
        at_menu::render_at_menu(frame, frame.area(), input_state);
    }
}
