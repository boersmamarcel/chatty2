use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

/// Manages the text input state
pub struct InputState {
    pub textarea: TextArea<'static>,
}

impl InputState {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message (Enter to send, Alt+Enter for newline) "),
        );
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        textarea.set_placeholder_style(Style::default().fg(Color::DarkGray));
        Self { textarea }
    }

    /// Get the current input text and clear the textarea
    pub fn take_input(&mut self) -> String {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n").trim().to_string();
        // Clear by selecting all and deleting
        self.textarea.select_all();
        self.textarea.cut();
        text
    }

    /// Get the current input text without clearing
    pub fn peek_input(&self) -> String {
        self.textarea.lines().join("\n").trim().to_string()
    }

    /// Check if the input is empty
    pub fn is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|l| l.trim().is_empty())
    }
}

pub fn render_input(frame: &mut Frame, area: Rect, input_state: &InputState) {
    frame.render_widget(&input_state.textarea, area);
}
