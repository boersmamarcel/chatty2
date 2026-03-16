use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

// ---------------------------------------------------------------------------
// @ mention / file picker
// ---------------------------------------------------------------------------

/// Directories excluded from the @ mention file list.
const AT_EXCLUDED: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
    ".git",
];

/// Maximum number of items shown in the @ mention picker.
const AT_MENU_MAX_ITEMS: usize = 15;

/// Load files from `dir` for the @ mention picker.
/// Returns a sorted list, skipping hidden entries and common build directories.
pub fn load_files_for_dir(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || AT_EXCLUDED.contains(&name.as_str()) {
                return None;
            }
            Some(name)
        })
        .collect();
    files.sort_unstable();
    files
}

/// Extract the `@` query from the end of `input_text`.
///
/// Returns `Some(query)` when the text ends with `@<word>` (no whitespace
/// after `@`).
pub fn at_query_from(input_text: &str) -> Option<String> {
    let last_line = input_text.lines().next_back().unwrap_or(input_text);
    let trimmed = last_line.trim_end();
    let at_pos = trimmed.rfind('@')?;
    let after_at = &trimmed[at_pos + 1..];
    if after_at.chars().any(char::is_whitespace) {
        return None;
    }
    Some(after_at.to_ascii_lowercase())
}

/// Return filtered items from `files` for the current `@` query in `input_text`.
pub fn at_menu_items_for<'a>(input_text: &str, files: &'a [String]) -> Vec<&'a String> {
    let Some(query) = at_query_from(input_text) else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|f| query.is_empty() || f.to_ascii_lowercase().contains(query.as_str()))
        .take(AT_MENU_MAX_ITEMS)
        .collect()
}

/// Build the replacement input text when a file is chosen from the @ picker.
pub fn apply_at_to_input(input_text: &str, filename: &str) -> String {
    let input_text = input_text.trim_end_matches(['\r', '\n']);
    let last_line_start = input_text.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let last_line = &input_text[last_line_start..];
    let at_pos_in_line = match last_line.rfind('@') {
        Some(p) => p,
        None => return format!("{} @{} ", input_text.trim_end(), filename),
    };
    let prefix_lines = &input_text[..last_line_start];
    let before_at = &last_line[..at_pos_in_line];
    if prefix_lines.is_empty() && before_at.is_empty() {
        format!("@{} ", filename)
    } else if before_at.is_empty() {
        format!("{}@{} ", prefix_lines, filename)
    } else {
        format!("{}{}@{} ", prefix_lines, before_at, filename)
    }
}

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

/// A combined item in the TUI slash-command picker: either a built-in command
/// or a dynamic filesystem skill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashMenuItem {
    Command(SlashCommandEntry),
    Skill { name: String, description: String },
}

impl SlashMenuItem {
    /// The slash-prefixed display string (e.g. `/compact` or `/fix-ci`).
    pub fn display_command(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.command.to_string(),
            SlashMenuItem::Skill { name, .. } => format!("/{}", name),
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &str {
        match self {
            SlashMenuItem::Command(cmd) => cmd.description,
            SlashMenuItem::Skill { description, .. } => description,
        }
    }

    /// Whether the item should be applied immediately (no arg input needed).
    pub fn execute_immediately(&self) -> bool {
        match self {
            SlashMenuItem::Command(cmd) => cmd.execute_immediately,
            SlashMenuItem::Skill { .. } => false,
        }
    }

    /// Text to insert into the input box when this item is selected.
    pub fn insert_text(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.insert_text.to_string(),
            SlashMenuItem::Skill { name, .. } => format!("Use the '{}' skill: ", name),
        }
    }

    /// Returns true when this item represents a filesystem skill.
    pub fn is_skill(&self) -> bool {
        matches!(self, SlashMenuItem::Skill { .. })
    }
}

/// Manages the text input state
pub struct InputState {
    pub textarea: TextArea<'static>,
    slash_menu_selected: usize,
    slash_menu_scroll_offset: usize,
    /// Filesystem skills loaded from the workspace `.claude/skills/` and global skills dirs.
    available_skills: Vec<(String, String)>,
    /// Cached list of files for the `@` mention picker.
    pub at_menu_files: Vec<String>,
    /// Index of the highlighted item in the `@` mention picker.
    at_menu_selected: usize,
    at_menu_scroll_offset: usize,
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
            slash_menu_scroll_offset: 0,
            available_skills: Vec::new(),
            at_menu_files: Vec::new(),
            at_menu_selected: 0,
            at_menu_scroll_offset: 0,
        }
    }

    /// Replace the cached list of filesystem skills.
    pub fn set_available_skills(&mut self, skills: Vec<(String, String)>) {
        self.available_skills = skills;
        self.slash_menu_selected = 0;
        self.slash_menu_scroll_offset = 0;
    }

    /// Get the current input text and clear the textarea
    pub fn take_input(&mut self) -> String {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n").trim().to_string();
        // Clear by selecting all and deleting
        self.textarea.select_all();
        self.textarea.cut();
        self.slash_menu_selected = 0;
        self.slash_menu_scroll_offset = 0;
        self.at_menu_selected = 0;
        self.at_menu_scroll_offset = 0;
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
        self.slash_menu_scroll_offset = 0;
        self.at_menu_selected = 0;
        self.at_menu_scroll_offset = 0;
    }

    /// Returns all matching slash-menu items for the current input: built-in commands
    /// first, then filesystem skills.
    pub fn slash_menu_items(&self) -> Vec<SlashMenuItem> {
        let Some(query) = self.slash_query() else {
            return Vec::new();
        };

        let mut items: Vec<SlashMenuItem> = SLASH_COMMANDS
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
            .map(SlashMenuItem::Command)
            .collect();

        let skill_items = self.available_skills.iter().filter(|(name, _)| {
            query.is_empty() || name.to_ascii_lowercase().starts_with(&query)
        });
        items.extend(skill_items.map(|(name, desc)| SlashMenuItem::Skill {
            name: name.clone(),
            description: desc.clone(),
        }));

        items
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

    pub fn selected_slash_menu_item(&mut self) -> Option<SlashMenuItem> {
        self.normalize_slash_menu_selection();
        self.slash_menu_items()
            .get(self.slash_menu_selected)
            .cloned()
    }

    pub fn slash_menu_selected_index(&self) -> usize {
        self.slash_menu_selected
    }

    pub fn slash_menu_scroll_offset(&self) -> usize {
        self.slash_menu_scroll_offset
    }

    pub fn set_slash_menu_scroll_offset(&mut self, offset: usize) {
        self.slash_menu_scroll_offset = offset;
    }

    fn normalize_slash_menu_selection(&mut self) {
        let len = self.slash_menu_items().len();
        if len == 0 {
            self.slash_menu_selected = 0;
            self.slash_menu_scroll_offset = 0;
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

    // -----------------------------------------------------------------------
    // @ mention / file picker helpers
    // -----------------------------------------------------------------------

    /// Return filtered items from the cached file list for the current input.
    pub fn at_menu_items(&self) -> Vec<&String> {
        let text = self.peek_input();
        at_menu_items_for(&text, &self.at_menu_files)
    }

    /// Whether the `@` mention picker should be shown.
    pub fn is_at_menu_open(&self) -> bool {
        !self.at_menu_items().is_empty()
    }

    pub fn move_at_menu_up(&mut self) {
        let len = self.at_menu_items().len();
        if len == 0 {
            self.at_menu_selected = 0;
            return;
        }
        if self.at_menu_selected > 0 {
            self.at_menu_selected -= 1;
        }
    }

    pub fn move_at_menu_down(&mut self) {
        let items = self.at_menu_items();
        if items.is_empty() {
            self.at_menu_selected = 0;
            return;
        }
        if self.at_menu_selected + 1 < items.len() {
            self.at_menu_selected += 1;
        }
    }

    pub fn at_menu_selected_index(&self) -> usize {
        self.at_menu_selected
    }

    pub fn at_menu_scroll_offset(&self) -> usize {
        self.at_menu_scroll_offset
    }

    pub fn set_at_menu_scroll_offset(&mut self, offset: usize) {
        self.at_menu_scroll_offset = offset;
    }

    pub fn selected_at_menu_item(&self) -> Option<String> {
        let items = self.at_menu_items();
        if items.is_empty() {
            return None;
        }
        let idx = self.at_menu_selected.min(items.len() - 1);
        Some(items[idx].clone())
    }

    /// Load files from `dir` if the cache is empty.
    pub fn ensure_at_files_loaded(&mut self, dir: &std::path::Path) {
        if self.at_menu_files.is_empty() {
            self.at_menu_files = load_files_for_dir(dir);
        }
    }

    /// Apply the currently highlighted `@` mention: replace the `@<query>`
    /// suffix with `@<filename> ` and return the new input text.
    pub fn apply_at_mention(&mut self) -> Option<String> {
        let selected = self.selected_at_menu_item()?;
        let current = self.peek_input();
        let new_text = apply_at_to_input(&current, &selected);
        self.at_menu_selected = 0;
        self.at_menu_scroll_offset = 0;
        Some(new_text)
    }
}

pub fn render_input(frame: &mut Frame, area: Rect, input_state: &InputState) {
    frame.render_widget(&input_state.textarea, area);
}

#[cfg(test)]
mod tests {
    use super::{
        InputState, SlashMenuItem, apply_at_to_input, at_menu_items_for, at_query_from,
    };

    fn has_command(items: &[SlashMenuItem], command: &str) -> bool {
        items.iter().any(|i| i.display_command() == command)
    }

    fn has_skill(items: &[SlashMenuItem], name: &str) -> bool {
        items.iter().any(|i| matches!(i, SlashMenuItem::Skill { name: n, .. } if n == name))
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

    #[test]
    fn skills_appear_in_slash_menu() {
        let mut input = InputState::new();
        input.set_available_skills(vec![
            ("fix-ci".to_string(), "Fix CI failures".to_string()),
            ("build-and-check".to_string(), "Run build pipeline".to_string()),
        ]);

        input.set_input_text("/");
        let all = input.slash_menu_items();
        assert!(has_skill(&all, "fix-ci"), "fix-ci skill should appear");
        assert!(has_skill(&all, "build-and-check"), "build-and-check skill should appear");

        // Filter: only "fix" prefix
        input.set_input_text("/fix");
        let filtered = input.slash_menu_items();
        assert!(has_skill(&filtered, "fix-ci"));
        assert!(!has_skill(&filtered, "build-and-check"));

        // Commands with space should close the menu
        input.set_input_text("/fix-ci extra");
        assert!(!input.is_slash_menu_open());
    }

    #[test]
    fn skill_insert_text() {
        let item = SlashMenuItem::Skill {
            name: "fix-ci".to_string(),
            description: "Fix CI".to_string(),
        };
        assert_eq!(item.insert_text(), "Use the 'fix-ci' skill: ");
        assert!(!item.execute_immediately());
        assert!(item.is_skill());
    }

    #[test]
    fn at_query_returns_none_when_no_at() {
        assert!(at_query_from("hello world").is_none());
        assert!(at_query_from("").is_none());
    }

    #[test]
    fn at_query_returns_query_after_at() {
        assert_eq!(at_query_from("@"), Some(String::new()));
        assert_eq!(at_query_from("@readme"), Some("readme".into()));
        assert_eq!(at_query_from("hello @src"), Some("src".into()));
    }

    #[test]
    fn at_query_closes_on_space() {
        assert!(at_query_from("@readme ").is_none());
        assert!(at_query_from("@readme.md and more").is_none());
    }

    #[test]
    fn at_menu_items_filter_by_query() {
        let files = vec![
            "README.md".to_string(),
            "src".to_string(),
            "Cargo.toml".to_string(),
        ];
        // "r" matches all three: README.md, src, Cargo.toml (all contain 'r')
        assert_eq!(at_menu_items_for("@r", &files).len(), 3);
        assert_eq!(at_menu_items_for("@", &files).len(), 3); // all
        assert_eq!(at_menu_items_for("@readme", &files).len(), 1); // only README.md
        assert!(at_menu_items_for("@zzz", &files).is_empty());
    }

    #[test]
    fn apply_at_to_input_replaces_query() {
        assert_eq!(apply_at_to_input("@", "README.md"), "@README.md ");
        assert_eq!(apply_at_to_input("@read", "README.md"), "@README.md ");
        assert_eq!(
            apply_at_to_input("please check @read", "README.md"),
            "please check @README.md "
        );
    }
}
