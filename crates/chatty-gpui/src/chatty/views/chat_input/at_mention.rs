//! `@` file-mention picker for the chat input.
//!
//! # What lives here
//!
//! - Pure helpers (no UI context required) are re-exported from
//!   `chatty_core::at_mention`, which is shared with the TUI (AGE-172):
//!   `load_files_for_dir`, `at_query_from`, `at_menu_items_for`,
//!   `apply_at_to_input`.
//! - `ChatInputState` methods that manage the picker's open/closed
//!   state, selection index, and file cache.
//! - `render_at_menu` — the popover element shown above the input.
//!
//! Helpers are `pub` (re-exported by `chat_input/mod.rs`) because the
//! `chat_input_test.rs` unit tests exercise them directly.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;

pub use chatty_core::at_mention::{
    apply_at_to_input, at_menu_items_for, at_query_from, load_files_for_dir,
};

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
        at_menu_items_for(text, &self.at_menu_files)
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

    /// Load files from `dir` if the cache is currently empty.
    pub fn ensure_at_files_loaded(&mut self, dir: &std::path::Path) {
        if self.at_menu_files.is_empty() {
            self.at_menu_files = load_files_for_dir(dir);
        }
    }

    /// If the `@` query is active (`input_text` ends with `@<word>`) and the
    /// file cache is empty, load files from the per-chat working directory
    /// (falling back to `global_dir`).  Returns `true` when the cache was
    /// populated for the first time (so the caller can trigger a re-render).
    pub fn refresh_at_files_if_needed(
        &mut self,
        input_text: &str,
        global_dir: Option<std::path::PathBuf>,
    ) -> bool {
        if at_query_from(input_text).is_none() || !self.at_menu_files.is_empty() {
            return false;
        }
        let dir = self.working_dir.clone().or(global_dir);
        if let Some(dir) = dir {
            self.ensure_at_files_loaded(&dir);
            return !self.at_menu_files.is_empty();
        }
        false
    }

    /// Return the number of `@` mention items matching the current `input_text`.
    /// Used by the keystroke interceptor without needing direct field access.
    pub fn at_items_count_for_input(&self, input_text: &str) -> usize {
        at_menu_items_for(input_text, &self.at_menu_files).len()
    }

    /// Apply the currently highlighted `@` mention.
    ///
    /// The `@<query>` suffix of the current input is replaced with
    /// `@<selected_filename> ` via `pending_at_insert`.
    pub fn apply_at_mention(&mut self, cx: &mut Context<Self>) {
        let input_text = self.input.read(cx).text().to_string();
        let items = at_menu_items_for(&input_text, &self.at_menu_files);
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
                    let filename_owned = filename.clone();
                    let is_selected = idx == selected.min(items.len().saturating_sub(1));

                    div()
                        .id(ElementId::Name(format!("at-mention-{}", idx).into()))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .gap_3()
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
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0x10b981))
                                .child(filename_owned),
                        )
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
