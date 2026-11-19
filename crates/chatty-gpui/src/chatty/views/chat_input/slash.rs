//! Slash-command and skill picker for the chat input.
//!
//! # What lives here
//!
//! - `SkillEntry` / `AgentPickerEntry` / `SlashMenuItem` (re-exported from
//!   `chatty_core::slash_commands`, shared with the TUI, see AGE-172/AGE-1323).
//! - `slash_menu_items_for` / `slash_menu_items_with_skills` /
//!   `agent_menu_items` / `slash_menu_items` — pure filtering helpers
//!   (also called from unit tests).
//! - `reachable_agents` — the agents the `/agent ` picker lists (AGE-761).
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
use std::path::Path;

use super::{ChatInputEvent, ChatInputState};
use crate::settings::models::{AgentSpecsModel, ExtensionsModel};
use chatty_core::settings::models::extensions_store::ExtensionKind;
use chatty_core::slash_commands::{MenuFrontend, command_and_skill_items};

// ---------------------------------------------------------------------------
// Slash command menu
// ---------------------------------------------------------------------------

pub use chatty_core::slash_commands::{AgentPickerEntry, SkillEntry, SlashMenuItem};

/// Returns the built-in slash commands that match the current `input_text`.
/// The menu is active only when `input_text` starts with `/` and contains
/// no whitespace (once there is a space the user is typing arguments).
///
/// Use [`slash_menu_items_with_skills`] to also include filesystem skills.
#[cfg(test)]
pub fn slash_menu_items_for(
    input_text: &str,
) -> Vec<&'static chatty_core::slash_commands::SlashCommandSpec> {
    let trimmed = input_text.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    // Once the user has typed a space (argument separator) close the menu.
    if trimmed.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let query = trimmed[1..].to_ascii_lowercase();
    chatty_core::slash_commands::gpui_commands()
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
    command_and_skill_items(&trimmed[1..], MenuFrontend::Gpui, skills)
}

pub use chatty_core::slash_commands::agent_menu_items;
use chatty_core::slash_commands::agent_query;

/// Every picker row for `input_text`: the `/agent ` picker's once it is in
/// play (AGE-761), else commands and skills.
pub fn slash_menu_items(
    input_text: &str,
    skills: &[SkillEntry],
    agents: &[AgentPickerEntry],
) -> Vec<SlashMenuItem> {
    agent_menu_items(input_text, agents)
        .unwrap_or_else(|| slash_menu_items_with_skills(input_text, skills))
}

/// The agents `/agent` can reach from a conversation whose effective
/// workspace is `workspace` (AGE-761): the local roster its `invoke_agent`
/// is given (`gateway_and_roster`), then the enabled remote A2A agents
/// (`ExtensionsModel`). A remote agent wins a name both have, as it does
/// when `/agent` dispatches.
pub fn reachable_agents(cx: &mut App, workspace: Option<&Path>) -> Vec<AgentPickerEntry> {
    // The same names the conversation's `invoke_agent` gets, so the picker
    // never offers an agent no broker can reach (AGE-759).
    let roster = crate::chatty::controllers::app_controller::gateway_and_roster(cx, workspace)
        .unwrap_or_default();
    // Enabled the way `/agent` dispatch reads it: the extension's own switch.
    let remote: Vec<AgentPickerEntry> = cx
        .try_global::<ExtensionsModel>()
        .map(|extensions| {
            extensions
                .extensions
                .iter()
                .filter(|extension| extension.enabled)
                .filter_map(|extension| match &extension.kind {
                    ExtensionKind::A2aAgent(config) => Some(AgentPickerEntry {
                        name: config.name.clone(),
                        description: extension.description.clone(),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let listings = if roster.is_empty() {
        Vec::new()
    } else {
        AgentSpecsModel::listings(cx)
    };
    let mut agents: Vec<AgentPickerEntry> = roster
        .into_iter()
        .filter(|name| !remote.iter().any(|agent| agent.name == *name))
        .map(|name| AgentPickerEntry {
            description: listings
                .iter()
                .find(|listing| listing.name == name && !listing.shadowed)
                .and_then(|listing| listing.spec.as_ref().ok())
                .and_then(|spec| spec.agent.description.clone())
                .unwrap_or_default(),
            name,
        })
        .collect();
    agents.extend(remote);
    agents
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
        !self.slash_menu_items(&text).is_empty()
    }

    /// The picker's rows for `text`: commands and skills, or the `/agent `
    /// picker's agents — none once Escape dismissed that picker and the
    /// text has not changed since (AGE-761).
    pub fn slash_menu_items(&self, text: &str) -> Vec<SlashMenuItem> {
        if agent_query(text).is_some()
            && self.agent_picker_dismissed_for.as_deref()
                == Some(text.trim_end_matches(['\n', '\r']))
        {
            return Vec::new();
        }
        slash_menu_items(text, &self.available_skills, &self.available_agents)
    }

    /// Whether the `/agent ` picker is showing for `text` (AGE-761).
    pub fn is_agent_picker_open(&self, text: &str) -> bool {
        agent_query(text).is_some() && !self.slash_menu_items(text).is_empty()
    }

    /// Escape on the `/agent ` picker: close it but keep what was typed
    /// (AGE-761). The next edit opens it again.
    pub fn dismiss_agent_picker(&mut self, text: &str) {
        self.agent_picker_dismissed_for = Some(text.trim_end_matches(['\n', '\r']).to_string());
    }

    /// Re-read the agents the `/agent ` picker offers each time it opens
    /// for `text` (AGE-761), for the conversation's own effective workspace.
    pub fn refresh_agents_if_picker_opened(&mut self, text: &str, cx: &mut Context<Self>) {
        let open = agent_query(text).is_some();
        if open && !self.agent_picker_open {
            let default_workspace = cx
                .try_global::<crate::settings::models::ExecutionSettingsModel>()
                .and_then(|settings| settings.workspace_dir.clone())
                .map(std::path::PathBuf::from);
            let workspace = chatty_core::agent_spec::roster_workspace(
                default_workspace.as_deref(),
                self.working_dir.as_deref(),
            )
            .map(Path::to_path_buf);
            self.available_agents = reachable_agents(cx, workspace.as_deref());
        }
        self.agent_picker_open = open;
    }

    /// Replace the agents the `/agent ` picker offers (AGE-761).
    #[cfg(test)]
    pub fn set_available_agents(&mut self, agents: Vec<AgentPickerEntry>, cx: &mut Context<Self>) {
        self.available_agents = agents;
        cx.notify();
    }

    /// Current highlighted index in the picker.
    pub fn slash_menu_selected(&self) -> usize {
        self.slash_menu.selected()
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
        // The `/agent ` picker's query keeps its own key: it can never
        // equal a command query, which has no space (AGE-761).
        let agent_key = agent_query(new_text).map(|query| format!("agent {query}"));
        let query_raw: &str = if let Some(key) = agent_key.as_deref() {
            key
        } else if trimmed.starts_with('/') && !trimmed.chars().any(char::is_whitespace) {
            &trimmed[1..]
        } else {
            ""
        };

        if self.slash_menu.set_filter(query_raw) {
            self.slash_menu_scroll_handle.scroll_to_item(0);
        }
    }

    /// Move selection up (wraps to last item).
    pub fn move_slash_menu_up(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        self.slash_menu.move_up(num_items);
        self.slash_menu_scroll_handle
            .scroll_to_item(self.slash_menu.selected());
    }

    /// Move selection down (wraps to first item).
    pub fn move_slash_menu_down(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        self.slash_menu.move_down(num_items);
        self.slash_menu_scroll_handle
            .scroll_to_item(self.slash_menu.selected());
    }

    /// Apply the currently highlighted slash command or skill.
    ///
    /// * For immediate commands (no args needed) the command is emitted via
    ///   `ChatInputEvent::SlashCommandSelected` and the input is cleared.
    /// * For argument commands (and all skills) the `insert_text` is written
    ///   into the input on the next render frame via `pending_slash_insert`.
    pub fn apply_slash_command(&mut self, cx: &mut Context<Self>) {
        let input_text = self.input.read(cx).text().to_string();
        let items = self.slash_menu_items(&input_text);
        if items.is_empty() {
            return;
        }
        let selected = self.slash_menu.selected_in(items.len());
        let item = &items[selected];
        if !item.is_selectable() {
            return;
        }
        self.slash_menu.clear(); // next '/' starts fresh
        self.slash_menu_scroll_handle.scroll_to_item(0);

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
    let theme_muted = cx.theme().muted_foreground;
    let agent_picker = items
        .iter()
        .any(|item| matches!(item, SlashMenuItem::Agent(_) | SlashMenuItem::NoAgents));

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
                    // The empty agent picker's placeholder: muted, and
                    // nothing to hover or click (AGE-761).
                    if !item.is_selectable() {
                        return div()
                            .id("slash-no-agents")
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(theme_muted)
                            .child(item.display_command());
                    }
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
                                    if s.slash_menu.selected() != idx {
                                        s.slash_menu.select(idx);
                                        s.slash_menu_scroll_handle.scroll_to_item(idx);
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                            state_for_click.update(cx, |s, cx| {
                                s.slash_menu.select(idx);
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
                .child(if agent_picker {
                    "↑↓ navigate  ·  Enter or Tab to insert  ·  Esc to dismiss"
                } else {
                    "↑↓ navigate  ·  Enter to apply  ·  Esc to dismiss"
                }),
        )
}
