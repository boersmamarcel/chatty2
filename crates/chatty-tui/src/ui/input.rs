use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders};
use tui_textarea::{AtomicRange, TextArea};

use crate::ui::theme;

// ---------------------------------------------------------------------------
// @ mention / file picker
// ---------------------------------------------------------------------------

// Pure helpers shared with the GPUI desktop app (AGE-172) — see
// `chatty_core::at_mention` for the shared source and its own unit tests.
#[cfg(test)]
pub use chatty_core::at_mention::at_menu_items_for;
pub use chatty_core::at_mention::{AtFiles, apply_at_to_input, at_query_from, load_files_for_dir};
use chatty_core::paste;

/// The slash menu's rows are shared with the GPUI desktop app
/// (`chatty_core::slash_commands`); the TUI only ever produces commands and
/// skills, never the `/agent ` picker's agent rows.
pub use chatty_core::slash_commands::SlashMenuItem;
use chatty_core::slash_commands::{MenuFrontend, MenuWrap, SkillEntry, SlashMenu};

/// Manages the text input state
pub struct InputState {
    pub textarea: TextArea<'static>,
    slash_menu: SlashMenu,
    /// Filesystem skills loaded from the project and global skill directories.
    available_skills: Vec<SkillEntry>,
    /// The `@` picker's index of the working folder (AGE-2192).
    pub at_menu_files: AtFiles,
    /// Whether the last key left an `@` query in play, so the index is
    /// reloaded only when the picker opens.
    at_menu_was_open: bool,
    /// A background re-index, taken in by [`Self::take_at_files_reload`].
    at_menu_reload: Option<std::sync::mpsc::Receiver<Vec<String>>>,
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
                .border_style(theme::border())
                .title(" Message (Enter to send, Alt+Enter for newline) "),
        );
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message...");
        textarea.set_placeholder_style(theme::muted());
        Self {
            textarea,
            slash_menu: SlashMenu::new(MenuWrap::Clamp),
            available_skills: Vec::new(),
            at_menu_files: AtFiles::default(),
            at_menu_was_open: false,
            at_menu_reload: None,
            at_menu_selected: 0,
            at_menu_scroll_offset: 0,
        }
    }

    /// Replace the cached list of filesystem skills.
    pub fn set_available_skills(&mut self, skills: Vec<(String, String)>) {
        self.available_skills = skills
            .into_iter()
            .map(|(name, description)| SkillEntry { name, description })
            .collect();
        self.slash_menu.reset();
    }

    /// Get the current input text and clear the textarea
    pub fn take_input(&mut self) -> String {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n").trim().to_string();
        // Drop the paste ranges before the buffer they point into goes away.
        self.textarea.clear_atomic_ranges();
        // Clear by selecting all and deleting
        self.textarea.select_all();
        self.textarea.cut();
        self.slash_menu.reset();
        self.at_menu_selected = 0;
        self.at_menu_scroll_offset = 0;
        text
    }

    /// Insert `text` at the cursor, as the paste store rendered it — either the
    /// paste itself or a `[Pasted text #N +M lines]` reference (AGE-341).
    pub fn insert_paste(&mut self, text: &str) {
        self.textarea.insert_str(text);
        self.sync_paste_ranges();
    }

    /// Mark every paste reference in the input atomic.
    ///
    /// The cursor then steps over a reference instead of into it, and
    /// backspace at its edge removes the whole thing — chewing one character
    /// off the end would leave a token that no longer resolves and would be
    /// sent to the model literally.
    pub fn sync_paste_ranges(&mut self) {
        let ranges: Vec<AtomicRange> = self
            .textarea
            .lines()
            .iter()
            .enumerate()
            .flat_map(|(row, line)| {
                paste::token_spans(line)
                    .into_iter()
                    .map(move |(start_col, end_col)| AtomicRange {
                        row,
                        start_col,
                        end_col,
                    })
            })
            .collect();

        // Ranges are derived from the buffer itself, so they are in bounds and
        // non-overlapping by construction; ignore the validation error rather
        // than panicking in the middle of a keystroke.
        let _ = self.textarea.try_set_atomic_ranges(ranges);
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
        self.textarea.clear_atomic_ranges();
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
        self.sync_paste_ranges();
        self.slash_menu.reset();
        self.at_menu_selected = 0;
        self.at_menu_scroll_offset = 0;
    }

    /// Returns all matching slash-menu items for the current input: built-in commands
    /// first, then filesystem skills.
    pub fn slash_menu_items(&self) -> Vec<SlashMenuItem> {
        let Some(query) = self.slash_query() else {
            return Vec::new();
        };

        chatty_core::slash_commands::command_and_skill_items(
            &query,
            MenuFrontend::Tui,
            &self.available_skills,
        )
    }

    pub fn is_slash_menu_open(&self) -> bool {
        !self.slash_menu_items().is_empty()
    }

    pub fn move_slash_menu_up(&mut self) {
        let len = self.slash_menu_items().len();
        self.slash_menu.move_up(len);
    }

    pub fn move_slash_menu_down(&mut self) {
        let len = self.slash_menu_items().len();
        self.slash_menu.move_down(len);
    }

    pub fn selected_slash_menu_item(&mut self) -> Option<SlashMenuItem> {
        let items = self.slash_menu_items();
        self.slash_menu.normalize(items.len());
        items.get(self.slash_menu.selected()).cloned()
    }

    pub fn slash_menu_selected_index(&self) -> usize {
        self.slash_menu.selected()
    }

    pub fn slash_menu_scroll_offset(&self) -> usize {
        self.slash_menu.offset()
    }

    pub fn set_slash_menu_scroll_offset(&mut self, offset: usize) {
        self.slash_menu.set_offset(offset);
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
        let text = self.textarea.lines().join("\n");
        self.at_menu_files.items_for(&text)
    }

    /// Whether the current input contains an `@` query (regardless of whether
    /// the file cache has been populated yet).
    pub fn has_at_query(&self) -> bool {
        let raw = self.textarea.lines().join("\n");
        at_query_from(&raw).is_some()
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

    /// When the input has just opened the `@` picker, index `dir`: right
    /// away the first time (nothing to show otherwise), else on a
    /// background thread while the previous index serves, so files made
    /// since show up without a large folder stalling the keystroke.
    pub fn refresh_at_files(&mut self, dir: &std::path::Path) {
        let open = self.has_at_query();
        let opened = open && !self.at_menu_was_open;
        self.at_menu_was_open = open;
        if !opened {
            return;
        }
        if self.at_menu_files.is_empty() {
            self.at_menu_files = AtFiles::new(load_files_for_dir(dir));
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(load_files_for_dir(&dir));
        });
        self.at_menu_reload = Some(rx);
    }

    /// Take in a finished background re-index; `true` when the index
    /// changed and the menu should be redrawn.
    pub fn take_at_files_reload(&mut self) -> bool {
        let Some(files) = self
            .at_menu_reload
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return false;
        };
        self.at_menu_reload = None;
        self.at_menu_files = AtFiles::new(files);
        true
    }

    /// Invalidate the cached `@` file list so it will be reloaded from the new
    /// working directory on the next `@` query.
    pub fn invalidate_at_files(&mut self) {
        self.at_menu_files = AtFiles::default();
        self.at_menu_reload = None;
        self.at_menu_was_open = false;
        self.at_menu_selected = 0;
        self.at_menu_scroll_offset = 0;
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
        AtFiles, InputState, SlashMenuItem, apply_at_to_input, at_menu_items_for, at_query_from,
    };

    fn has_command(items: &[SlashMenuItem], command: &str) -> bool {
        items.iter().any(|i| i.display_command() == command)
    }

    fn has_skill(items: &[SlashMenuItem], name: &str) -> bool {
        items
            .iter()
            .any(|i| matches!(i, SlashMenuItem::Skill(s) if s.name == name))
    }

    #[test]
    fn slash_menu_opens_for_slash_input_and_filters() {
        let mut input = InputState::new();
        input.set_input_text("/");
        let all = input.slash_menu_items();
        assert!(!all.is_empty());
        assert!(has_command(&all, "/compact"));
        assert!(has_command(&all, "/modules"));

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
            (
                "build-and-check".to_string(),
                "Run build pipeline".to_string(),
            ),
        ]);

        input.set_input_text("/");
        let all = input.slash_menu_items();
        assert!(has_skill(&all, "fix-ci"), "fix-ci skill should appear");
        assert!(
            has_skill(&all, "build-and-check"),
            "build-and-check skill should appear"
        );

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
        let item = SlashMenuItem::Skill(chatty_core::slash_commands::SkillEntry {
            name: "fix-ci".to_string(),
            description: "Fix CI".to_string(),
        });
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
    fn has_at_query_is_true_when_at_is_typed_even_with_empty_file_cache() {
        // This is the key regression test for the original bug:
        // has_at_query() must return true when the input contains `@`
        // even when at_menu_files is empty (before any files are loaded).
        let mut input = InputState::new();
        assert!(input.at_menu_files.is_empty(), "files start empty");

        // No @ -> false
        input.set_input_text("hello");
        assert!(!input.has_at_query());

        // Just @ -> true (even with no files loaded)
        input.set_input_text("@");
        assert!(input.has_at_query());
        // But is_at_menu_open() is false because files haven't been loaded yet
        assert!(!input.is_at_menu_open());

        // With partial query -> still true
        input.set_input_text("@readme");
        assert!(input.has_at_query());

        // After space -> false (query closed)
        input.set_input_text("@readme ");
        assert!(!input.has_at_query());
    }

    #[test]
    fn is_at_menu_open_after_files_loaded() {
        let mut input = InputState::new();
        input.at_menu_files = AtFiles::new(vec!["README.md".to_string(), "src".to_string()]);
        input.set_input_text("@");
        assert!(input.is_at_menu_open());

        input.set_input_text("@readme");
        assert!(input.is_at_menu_open());

        input.set_input_text("@zzz_no_match");
        assert!(!input.is_at_menu_open());
    }

    #[test]
    fn invalidate_at_files_clears_cache() {
        let mut input = InputState::new();
        input.at_menu_files = AtFiles::new(vec!["file.txt".to_string()]);
        input.at_menu_selected = 2;
        input.at_menu_scroll_offset = 5;
        input.invalidate_at_files();
        assert!(input.at_menu_files.is_empty());
        assert_eq!(input.at_menu_selected_index(), 0);
        assert_eq!(input.at_menu_scroll_offset(), 0);
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

    /// The reference behaves as one character: the cursor never lands inside
    /// it, and backspace at its edge takes all of it. Half a token would be
    /// sent to the model as literal text.
    #[test]
    fn a_paste_reference_is_deleted_whole() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut input = InputState::new();
        input.insert_paste("look: ");
        input.insert_paste("[Pasted text #1 +45 lines]");
        assert_eq!(input.peek_input(), "look: [Pasted text #1 +45 lines]");

        let ranges = input.textarea.atomic_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_col, "look: ".chars().count());

        input
            .textarea
            .input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
        input.sync_paste_ranges();

        assert_eq!(input.peek_input(), "look:");
        assert!(input.textarea.atomic_ranges().is_empty());
    }

    #[test]
    fn taking_the_input_drops_the_paste_ranges() {
        let mut input = InputState::new();
        input.insert_paste("[Pasted text #1 +45 lines]");

        assert_eq!(input.take_input(), "[Pasted text #1 +45 lines]");
        assert!(input.textarea.atomic_ranges().is_empty());
        assert!(input.is_empty());
    }
}
