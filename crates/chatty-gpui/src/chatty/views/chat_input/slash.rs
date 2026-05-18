//! Slash-command and skill picker for the chat input.
//!
//! # What lives here
//!
//! - `SlashCommand` / `SkillEntry` / `SlashMenuItem` types.
//! - `slash_menu_items_for` / `slash_menu_items_with_skills` — pure
//!   filtering helpers (also called from `chat_view` and from unit
//!   tests).
//! - `ChatInputState` methods that manage the picker's open/closed
//!   state, selection index, and command application.
//! - `render_slash_menu` — the popover element shown above the input.
//!
//! Items are `pub` and re-exported by `chat_input/mod.rs` so external
//! callers and tests see the same surface as before the split.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

use super::{ChatInputEvent, ChatInputState};

// ---------------------------------------------------------------------------
// Slash command menu — types and filters
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Slash command menu
// ---------------------------------------------------------------------------

/// A single entry in the slash-command picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub command: &'static str,
    pub description: &'static str,
    /// Text that is inserted into the input when the command is selected.
    pub insert_text: &'static str,
    /// When true the command is sent immediately on selection; when false
    /// the insert_text is placed into the input so the user can add args.
    pub execute_immediately: bool,
}

/// A skill loaded from the filesystem for display in the slash-command picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillEntry {
    /// Short directory name of the skill (e.g. `"fix-ci"`).
    pub name: String,
    /// Human-readable description extracted from the skill's frontmatter.
    pub description: String,
}

/// A combined item in the slash-command picker: either a built-in command or a
/// dynamic skill loaded from the filesystem.
#[derive(Clone, Debug, PartialEq)]
pub enum SlashMenuItem {
    Command(&'static SlashCommand),
    Skill(SkillEntry),
}

impl SlashMenuItem {
    /// The slash-prefixed display string shown in the menu (e.g. `/compact` or `/fix-ci`).
    pub fn display_command(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.command.to_string(),
            SlashMenuItem::Skill(skill) => format!("/{}", skill.name),
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &str {
        match self {
            SlashMenuItem::Command(cmd) => cmd.description,
            SlashMenuItem::Skill(skill) => &skill.description,
        }
    }

    /// Whether the item should be applied immediately (no arg input needed).
    pub fn execute_immediately(&self) -> bool {
        match self {
            SlashMenuItem::Command(cmd) => cmd.execute_immediately,
            // Skills are not execute-immediately — we insert a prompt the user
            // can review and optionally extend before pressing Enter.
            SlashMenuItem::Skill(_) => false,
        }
    }

    /// Text to insert into the input when this item is selected.
    pub fn insert_text(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.insert_text.to_string(),
            SlashMenuItem::Skill(skill) => format!("Use the '{}' skill: ", skill.name),
        }
    }

    /// Returns true when this item represents a filesystem skill.
    pub fn is_skill(&self) -> bool {
        matches!(self, SlashMenuItem::Skill(_))
    }
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        command: "/add-dir",
        description: "Add a directory to allowed workspace access",
        insert_text: "/add-dir ",
        execute_immediately: false,
    },
    SlashCommand {
        command: "/agent",
        description: "Launch a sub-agent with a prompt",
        insert_text: "/agent ",
        execute_immediately: false,
    },
    SlashCommand {
        command: "/clear",
        description: "Clear conversation history",
        insert_text: "/clear",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/new",
        description: "Start a new conversation",
        insert_text: "/new",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/compact",
        description: "Summarize conversation history to reduce context",
        insert_text: "/compact",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/context",
        description: "Show context window usage",
        insert_text: "/context",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/copy",
        description: "Copy latest response to clipboard",
        insert_text: "/copy",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/cwd",
        description: "Show current working directory",
        insert_text: "/cwd",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/cd",
        description: "Change working directory",
        insert_text: "/cd ",
        execute_immediately: false,
    },
];

/// Returns the built-in slash commands that match the current `input_text`.
/// The menu is active only when `input_text` starts with `/` and contains
/// no whitespace (once there is a space the user is typing arguments).
///
/// Use [`slash_menu_items_with_skills`] to also include filesystem skills.
#[cfg(test)]
pub fn slash_menu_items_for(input_text: &str) -> Vec<&'static SlashCommand> {
    let trimmed = input_text.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    // Once the user has typed a space (argument separator) close the menu.
    if trimmed.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let query = trimmed[1..].to_ascii_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|cmd| {
            query.is_empty()
                || cmd
                    .command
                    .trim_start_matches('/')
                    .to_ascii_lowercase()
                    .starts_with(&query)
        })
        .collect()
}

/// Returns combined slash-menu items: built-in commands first, then filesystem
/// skills — both filtered to match the current query in `input_text`.
pub fn slash_menu_items_with_skills(input_text: &str, skills: &[SkillEntry]) -> Vec<SlashMenuItem> {
    let trimmed = input_text.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let query = trimmed[1..].to_ascii_lowercase();

    let mut items: Vec<SlashMenuItem> = SLASH_COMMANDS
        .iter()
        .filter(|cmd| {
            query.is_empty()
                || cmd
                    .command
                    .trim_start_matches('/')
                    .to_ascii_lowercase()
                    .starts_with(&query)
        })
        .map(SlashMenuItem::Command)
        .collect();

    let skill_items = skills
        .iter()
        .filter(|skill| query.is_empty() || skill.name.to_ascii_lowercase().starts_with(&query));
    items.extend(skill_items.map(|s| SlashMenuItem::Skill(s.clone())));

    items
}

// ---------------------------------------------------------------------------
// ChatInputState — slash menu state methods
// ---------------------------------------------------------------------------

impl ChatInputState {
    // -----------------------------------------------------------------------
    // Slash-command menu helpers
    // -----------------------------------------------------------------------

    /// Whether the slash-command picker should be shown given the current input.
    pub fn is_slash_menu_open(&self, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).text().to_string();
        !slash_menu_items_with_skills(&text, &self.available_skills).is_empty()
    }

    /// Current highlighted index in the picker.
    pub fn slash_menu_selected(&self) -> usize {
        self.slash_menu_selected
    }

    /// Reset the selection to 0 **only** when the slash query text changes.
    ///
    /// This is called from the `InputEvent::Change` subscriber.  We deliberately
    /// ignore spurious Change events that don't alter the query (e.g. the
    /// newline that gpui-component appends to the buffer right before it fires
    /// `InputEvent::PressEnter` in an auto-grow input) so that the arrow-key
    /// selection is still respected when the user presses Enter.
    pub fn reset_slash_menu_selection_if_query_changed(&mut self, new_text: &str) {
        // Extract the raw query slice (text after '/', no leading slash).
        // If there is no leading '/' or the text already contains whitespace
        // (menu would be closed anyway), treat as no active query.
        let trimmed = new_text.trim();
        let query_raw: &str =
            if trimmed.starts_with('/') && !trimmed.chars().any(char::is_whitespace) {
                &trimmed[1..]
            } else {
                ""
            };

        // Compare without allocating; only convert to owned when storing.
        let changed = self
            .last_slash_query
            .as_deref()
            .map(|prev| !prev.eq_ignore_ascii_case(query_raw))
            .unwrap_or(true);

        if changed {
            self.slash_menu_selected = 0;
            self.slash_menu_scroll_handle.scroll_to_item(0);
            self.last_slash_query = Some(query_raw.to_ascii_lowercase());
        }
    }

    /// Move selection up (wraps to last item).
    pub fn move_slash_menu_up(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        if self.slash_menu_selected == 0 {
            self.slash_menu_selected = num_items - 1;
        } else {
            self.slash_menu_selected -= 1;
        }
        self.slash_menu_scroll_handle
            .scroll_to_item(self.slash_menu_selected);
    }

    /// Move selection down (wraps to first item).
    pub fn move_slash_menu_down(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        self.slash_menu_selected = (self.slash_menu_selected + 1) % num_items;
        self.slash_menu_scroll_handle
            .scroll_to_item(self.slash_menu_selected);
    }

    /// Apply the currently highlighted slash command or skill.
    ///
    /// * For immediate commands (no args needed) the command is emitted via
    ///   `ChatInputEvent::SlashCommandSelected` and the input is cleared.
    /// * For argument commands (and all skills) the `insert_text` is written
    ///   into the input on the next render frame via `pending_slash_insert`.
    pub fn apply_slash_command(&mut self, cx: &mut Context<Self>) {
        let input_text = self.input.read(cx).text().to_string();
        let items = slash_menu_items_with_skills(&input_text, &self.available_skills);
        if items.is_empty() {
            return;
        }
        let selected = self.slash_menu_selected.min(items.len().saturating_sub(1));
        let item = &items[selected];
        self.slash_menu_selected = 0;
        self.slash_menu_scroll_handle.scroll_to_item(0);
        self.last_slash_query = None; // reset so next '/' starts fresh

        if item.execute_immediately() {
            // Only built-in commands reach here (skills are never immediate).
            if let SlashMenuItem::Command(cmd) = item {
                cx.emit(ChatInputEvent::SlashCommandSelected(
                    cmd.command.to_string(),
                ));
            }
            self.should_clear = true;
        } else {
            // Insert command text (with trailing space) so user can type args.
            self.pending_slash_insert = Some(item.insert_text());
        }
    }
}

// ---------------------------------------------------------------------------
// Slash menu renderer
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

/// Renders the slash-command picker above the input.
///
/// Built-in commands keep their description visible, while skills only show the
/// slash-prefixed skill name to avoid horizontal overflow in the popover.
pub(super) fn render_slash_menu(
    items: &[SlashMenuItem],
    selected: usize,
    state: &Entity<ChatInputState>,
    scroll_handle: &ScrollHandle,
    cx: &App,
) -> impl IntoElement {
    let theme_bg = cx.theme().background;
    let theme_border = cx.theme().border;
    let theme_secondary = cx.theme().secondary;

    div()
        .w_full()
        .flex()
        .flex_col()
        .bg(theme_bg)
        .border_1()
        .border_color(theme_border)
        .rounded_lg()
        .shadow_md()
        .p_1()
        .child(
            div()
                .id("slash-menu-items")
                .max_h(px(320.0))
                .track_scroll(scroll_handle)
                .overflow_y_scroll()
                .children(items.iter().enumerate().map(|(idx, item)| {
                    let state_for_click = state.clone();
                    let display_command = item.display_command();
                    let description = item.description().to_string();
                    let is_skill = item.is_skill();
                    let is_selected = idx == selected.min(items.len().saturating_sub(1));

                    // Skills use a purple accent; commands use the standard blue.
                    let command_color = if is_skill {
                        rgb(0x8b5cf6)
                    } else {
                        rgb(0x3b82f6)
                    };

                    div()
                        .id(ElementId::Name(
                            format!("slash-cmd-{}", display_command).into(),
                        ))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .gap_3()
                        .when(is_selected, |d| d.bg(theme_secondary))
                        .hover(|style| style.bg(theme_secondary))
                        // Highlight on hover to update selected index
                        .on_mouse_move({
                            let state = state.clone();
                            move |_event, _window, cx| {
                                state.update(cx, |s, cx| {
                                    if s.slash_menu_selected != idx {
                                        s.slash_menu_selected = idx;
                                        s.slash_menu_scroll_handle.scroll_to_item(idx);
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                            state_for_click.update(cx, |s, cx| {
                                s.slash_menu_selected = idx;
                                s.slash_menu_scroll_handle.scroll_to_item(idx);
                                s.apply_slash_command(cx);
                                cx.notify();
                            });
                        })
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(command_color)
                                .child(display_command),
                        )
                        .when(!is_skill, |d| {
                            d.child(div().text_sm().text_color(rgb(0x6b7280)).child(description))
                        })
                })),
        )
        .vertical_scrollbar(scroll_handle)
        .child(
            // Help footer
            div()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(rgb(0x9ca3af))
                .child("↑↓ navigate  ·  Enter to apply  ·  Esc to dismiss"),
        )
}
