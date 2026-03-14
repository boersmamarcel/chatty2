use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommandEntry {
    pub command: &'static str,
    pub description: &'static str,
    pub insert_text: &'static str,
    pub execute_immediately: bool,
}

const SLASH_COMMANDS: &[SlashCommandEntry] = &[
    SlashCommandEntry {
        command: "/model",
        description: "Switch model or open model picker",
        insert_text: "/model",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/tools",
        description: "Toggle tools or open tool picker",
        insert_text: "/tools",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/add-dir",
        description: "Add a directory to allowed workspace access",
        insert_text: "/add-dir ",
        execute_immediately: false,
    },
    SlashCommandEntry {
        command: "/agent",
        description: "Launch a sub-agent with a prompt",
        insert_text: "/agent ",
        execute_immediately: false,
    },
    SlashCommandEntry {
        command: "/clear",
        description: "Clear conversation history",
        insert_text: "/clear",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/new",
        description: "Start a new conversation",
        insert_text: "/new",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/compact",
        description: "Summarize conversation history",
        insert_text: "/compact",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/context",
        description: "Show context window usage",
        insert_text: "/context",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/copy",
        description: "Copy latest response to clipboard",
        insert_text: "/copy",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/cwd",
        description: "Show current working directory",
        insert_text: "/cwd",
        execute_immediately: true,
    },
    SlashCommandEntry {
        command: "/cd",
        description: "Change working directory",
        insert_text: "/cd ",
        execute_immediately: false,
    },
];

/// Manages the text input state
pub struct InputState {
    pub textarea: TextArea<'static>,
    slash_menu_selected: usize,
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
        Self {
            textarea,
            slash_menu_selected: 0,
        }
    }

    /// Get the current input text and clear the textarea
    pub fn take_input(&mut self) -> String {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n").trim().to_string();
        // Clear by selecting all and deleting
        self.textarea.select_all();
        self.textarea.cut();
        self.slash_menu_selected = 0;
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

    pub fn set_input_text(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
        self.slash_menu_selected = 0;
    }

    pub fn slash_menu_items(&self) -> Vec<SlashCommandEntry> {
        let Some(query) = self.slash_query() else {
            return Vec::new();
        };

        SLASH_COMMANDS
            .iter()
            .copied()
            .filter(|item| {
                query.is_empty()
                    || item
                        .command
                        .trim_start_matches('/')
                        .to_ascii_lowercase()
                        .starts_with(&query)
            })
            .collect()
    }

    pub fn is_slash_menu_open(&self) -> bool {
        !self.slash_menu_items().is_empty()
    }

    pub fn move_slash_menu_up(&mut self) {
        self.normalize_slash_menu_selection();
        if self.slash_menu_selected > 0 {
            self.slash_menu_selected -= 1;
        }
    }

    pub fn move_slash_menu_down(&mut self) {
        self.normalize_slash_menu_selection();
        let items = self.slash_menu_items();
        if self.slash_menu_selected + 1 < items.len() {
            self.slash_menu_selected += 1;
        }
    }

    pub fn selected_slash_menu_item(&mut self) -> Option<SlashCommandEntry> {
        self.normalize_slash_menu_selection();
        self.slash_menu_items()
            .get(self.slash_menu_selected)
            .copied()
    }

    pub fn slash_menu_selected_index(&self) -> usize {
        self.slash_menu_selected
    }

    fn normalize_slash_menu_selection(&mut self) {
        let len = self.slash_menu_items().len();
        if len == 0 {
            self.slash_menu_selected = 0;
        } else if self.slash_menu_selected >= len {
            self.slash_menu_selected = len - 1;
        }
    }

    fn slash_query(&self) -> Option<String> {
        let trimmed = self.peek_input();
        if !trimmed.starts_with('/') {
            return None;
        }

        let query = trimmed.trim_start_matches('/');
        if query.chars().any(char::is_whitespace) {
            return None;
        }

        Some(query.to_ascii_lowercase())
    }
}

pub fn render_input(frame: &mut Frame, area: Rect, input_state: &InputState) {
    frame.render_widget(&input_state.textarea, area);
}

#[cfg(test)]
mod tests {
    use super::{InputState, SlashCommandEntry};

    fn has_command(items: &[SlashCommandEntry], command: &str) -> bool {
        items.iter().any(|i| i.command == command)
    }

    #[test]
    fn slash_menu_opens_for_slash_input_and_filters() {
        let mut input = InputState::new();
        input.set_input_text("/");
        let all = input.slash_menu_items();
        assert!(!all.is_empty());
        assert!(has_command(&all, "/compact"));

        input.set_input_text("/com");
        let filtered = input.slash_menu_items();
        assert!(has_command(&filtered, "/compact"));
        assert!(!has_command(&filtered, "/context"));
        assert!(!has_command(&filtered, "/copy"));

        input.set_input_text("/compact now");
        assert!(!input.is_slash_menu_open());
    }
}
