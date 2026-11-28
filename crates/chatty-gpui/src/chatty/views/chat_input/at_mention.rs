//! `@` file-mention picker for the chat input.
//!
//! # What lives here
//!
//! - Pure helpers (no UI context required) are re-exported from
//!   `chatty_core::at_mention`, which is shared with the TUI (AGE-172):
//!   `AtFiles`, `load_files_for_dir`, `at_query_from`,
//!   `apply_at_to_input` (and `at_menu_items_for` for the tests).
//! - `ChatInputState` methods that manage the picker's open/closed
//!   state, selection index, and file index (reloaded off the UI thread
//!   each time the picker opens, AGE-2192).
//! - `render_at_menu` — the popover element shown above the input.
//!
//! Helpers are `pub` (re-exported by `chat_input/mod.rs`) because the
//! `chat_input_test.rs` unit tests exercise them directly.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

#[cfg(test)]
pub use chatty_core::at_mention::at_menu_items_for;
pub use chatty_core::at_mention::{AtFiles, apply_at_to_input, at_query_from, load_files_for_dir};

use super::ChatInputState;

// ---------------------------------------------------------------------------
// @ mention / file picker
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// ChatInputState — @ mention state methods
// ---------------------------------------------------------------------------

impl ChatInputState {
    // -----------------------------------------------------------------------
    // @ mention / file picker helpers
    // -----------------------------------------------------------------------

    /// Whether the `@` mention picker should be shown given the current input.
    pub fn is_at_menu_open(&self, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).text().to_string();
        !self.at_menu_items_for_text(&text).is_empty()
    }

    /// Return filtered file items for `text` using the cached file list.
    pub fn at_menu_items_for_text<'a>(&'a self, text: &str) -> Vec<&'a String> {
        self.at_menu_files.items_for(text)
    }

    /// Current highlighted index in the `@` picker.
    pub fn at_menu_selected(&self) -> usize {
        self.at_menu_selected
    }

    /// Reset the `@` selection to 0 only when the query text actually changes.
    pub fn reset_at_menu_selection_if_query_changed(&mut self, new_text: &str) {
        let query_raw = at_query_from(new_text).unwrap_or_default();
        let changed = self
            .last_at_query
            .as_deref()
            .map(|prev| !prev.eq_ignore_ascii_case(&query_raw))
            .unwrap_or(true);
        if changed {
            self.at_menu_selected = 0;
            self.at_menu_scroll_handle.scroll_to_item(0);
            self.last_at_query = Some(query_raw);
        }
    }

    /// Move `@` selection up (wraps to last item).
    pub fn move_at_menu_up(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        if self.at_menu_selected == 0 {
            self.at_menu_selected = num_items - 1;
        } else {
            self.at_menu_selected -= 1;
        }
        self.at_menu_scroll_handle
            .scroll_to_item(self.at_menu_selected);
    }

    /// Move `@` selection down (wraps to first item).
    pub fn move_at_menu_down(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        self.at_menu_selected = (self.at_menu_selected + 1) % num_items;
        self.at_menu_scroll_handle
            .scroll_to_item(self.at_menu_selected);
    }

    /// When `input_text` opens the `@` picker (an `@` query appears),
    /// re-index the per-chat working directory (falling back to
    /// `global_dir`) in the background, so files made since the last time
    /// show up. The previous index serves until the new one is in; the
    /// view re-renders when it is.
    pub fn refresh_at_files_if_needed(
        &mut self,
        input_text: &str,
        global_dir: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let open = at_query_from(input_text).is_some();
        let opened = open && !self.at_menu_was_open;
        self.at_menu_was_open = open;
        if !opened {
            return;
        }
        let Some(dir) = self.working_dir.clone().or(global_dir) else {
            return;
        };
        let walk = cx.background_spawn(async move { load_files_for_dir(&dir) });
        self.at_menu_load = Some(cx.spawn(async move |this, cx| {
            let files = walk.await;
            let _ = this.update(cx, |state, cx| {
                state.at_menu_files = AtFiles::new(files);
                cx.notify();
            });
        }));
    }

    /// Return the number of `@` mention items matching the current `input_text`.
    /// Used by the keystroke interceptor without needing direct field access.
    pub fn at_items_count_for_input(&self, input_text: &str) -> usize {
        self.at_menu_files.items_for(input_text).len()
    }

    /// Apply the currently highlighted `@` mention.
    ///
    /// The `@<query>` suffix of the current input is replaced with
    /// `@<selected_filename> ` via `pending_at_insert`.
    pub fn apply_at_mention(&mut self, cx: &mut Context<Self>) {
        let input_text = self.input.read(cx).text().to_string();
        let items = self.at_menu_files.items_for(&input_text);
        if items.is_empty() {
            return;
        }
        let selected = self.at_menu_selected.min(items.len().saturating_sub(1));
        let filename = items[selected].clone();
        let new_text = apply_at_to_input(&input_text, &filename);
        self.at_menu_selected = 0;
        self.at_menu_scroll_handle.scroll_to_item(0);
        self.last_at_query = None;
        // Retain the file cache so that the user can insert multiple files
        // in quick succession without reloading. The cache is cleared when the
        // working directory changes via `set_working_dir`.
        self.pending_at_insert = Some(new_text);
    }
}

// ---------------------------------------------------------------------------
// @ mention menu renderer
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// @ mention menu renderer
// ---------------------------------------------------------------------------

/// An index entry as the picker shows it: the name (a folder keeps its `/`)
/// and, dimmed after it, the folder it is in (empty at the top).
pub fn split_entry(entry: &str) -> (String, String) {
    let trimmed = entry.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(at) => (entry[at + 1..].to_string(), entry[..at].to_string()),
        None => (entry.to_string(), String::new()),
    }
}

/// Renders the `@` file mention picker above the input.
pub(super) fn render_at_menu(
    items: &[String],
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
                .id("at-menu-items")
                .max_h(px(320.0))
                .track_scroll(scroll_handle)
                .overflow_y_scroll()
                .children(items.iter().enumerate().map(|(idx, filename)| {
                    let state_for_click = state.clone();
                    let (name, folder) = split_entry(filename);
                    let is_selected = idx == selected.min(items.len().saturating_sub(1));

                    div()
                        .id(ElementId::Name(format!("at-mention-{}", idx).into()))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_baseline()
                        .gap_2()
                        .when(is_selected, |d| d.bg(theme_secondary))
                        .hover(|style| style.bg(theme_secondary))
                        .on_mouse_move({
                            let state = state.clone();
                            move |_event, _window, cx| {
                                state.update(cx, |s, cx| {
                                    if s.at_menu_selected != idx {
                                        s.at_menu_selected = idx;
                                        s.at_menu_scroll_handle.scroll_to_item(idx);
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                            state_for_click.update(cx, |s, cx| {
                                s.at_menu_selected = idx;
                                s.at_menu_scroll_handle.scroll_to_item(idx);
                                s.apply_at_mention(cx);
                                cx.notify();
                            });
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0x10b981))
                                .child(name),
                        )
                        .when(!folder.is_empty(), |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(0x9ca3af))
                                    .child(folder),
                            )
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
                .child("↑↓ navigate  ·  Enter to insert  ·  Esc to dismiss"),
        )
}

#[cfg(test)]
mod tests {
    use super::split_entry;

    #[test]
    fn split_entry_shows_the_name_then_its_folder() {
        assert_eq!(
            split_entry("README.md"),
            ("README.md".into(), String::new())
        );
        assert_eq!(
            split_entry("src/ui/app.rs"),
            ("app.rs".into(), "src/ui".into())
        );
        assert_eq!(split_entry("src/ui/"), ("ui/".into(), "src".into()));
    }
}
