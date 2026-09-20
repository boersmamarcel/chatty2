//! Cmd/Ctrl+P quick-open (AGE-480): a fuzzy picker over every file in the
//! active workspace, respecting the same `.git`-hidden rule as the sidebar's
//! tree. Selecting an entry opens it as an artifact tab — the zero-layout
//! cost path for "open the file the agent just wrote", and the fallback
//! when the sidebar is collapsed.
//!
//! Structurally this mirrors [`super::search_conversations_dialog`]: a
//! stateful view holding the input and a snapshot taken when the dialog
//! opened, filtered live as the query changes. Escape closes it via the
//! `Dialog` component's own binding (see `gpui_component::dialog`); there is
//! nothing to wire up here for that.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, WindowExt, h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    v_flex,
};

use crate::chatty::controllers::GlobalChattyApp;
use crate::chatty::views::sidebar_view::active_workspace_root;
use crate::chatty::views::transcript::file_tree::walk_files;
use crate::chatty::views::transcript::read_artifact_source;

/// How many ranked matches are shown; the workspace can have thousands of
/// files and only the top handful are ever useful.
const MAX_RESULTS: usize = 200;

/// A minimal fuzzy match: every character of `needle` must appear in
/// `haystack` in order, case-insensitively. The score rewards long
/// contiguous runs and an earlier first match — plain fuzzy ranking on the
/// relative path is all v1 asks for (no recency weighting).
fn fuzzy_score(needle: &str, haystack: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay_lower: Vec<char> = haystack.to_lowercase().chars().collect();
    let mut search_from = 0usize;
    let mut first_match: Option<usize> = None;
    let mut last_matched: Option<usize> = None;
    let mut run: i64 = 0;
    let mut best_run: i64 = 0;
    for needle_char in needle.to_lowercase().chars() {
        let found = hay_lower[search_from..]
            .iter()
            .position(|&c| c == needle_char)
            .map(|ix| ix + search_from)?;
        if first_match.is_none() {
            first_match = Some(found);
        }
        run = if last_matched == Some(found.wrapping_sub(1)) {
            run + 1
        } else {
            1
        };
        best_run = best_run.max(run);
        last_matched = Some(found);
        search_from = found + 1;
    }
    let first = first_match.unwrap_or(0) as i64;
    Some(best_run * 100 - first)
}

/// Open `path` as an artifact tab through the live `ChattyApp`, the way
/// [`super::sidebar_view::SidebarEvent::OpenFile`] does for the sidebar's
/// tree — the quick-open picker has no sidebar entity to route an event
/// through, so it reaches the artifact panel directly.
fn open_as_artifact(path: PathBuf, root: &std::path::Path, cx: &mut App) {
    let Some(app) = cx
        .try_global::<GlobalChattyApp>()
        .and_then(|g| g.try_upgrade())
    else {
        return;
    };
    let source = read_artifact_source(&path);
    let workspace = Some(root.display().to_string());
    app.update(cx, |app, cx| {
        let artifact_view = app.chat_view.read(cx).artifact_view().clone();
        artifact_view.update(cx, |view, cx| {
            view.open_from_sidebar(path, source, workspace, cx);
        });
    });
}

struct QuickOpenView {
    input: Entity<InputState>,
    query: String,
    root: PathBuf,
    /// Every file under `root` at the moment the dialog opened; workspaces
    /// large enough for this snapshot to go stale mid-session are rare
    /// enough that a fresh Cmd+P (which re-walks) is an adequate fix.
    all: Vec<PathBuf>,
    _sub: Subscription,
}

impl QuickOpenView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to file…"));
        let root = active_workspace_root(cx);
        let all = walk_files(&root);

        let _sub = cx.subscribe(
            &input,
            |this: &mut Self, input, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.query = input.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.open_first_match(cx);
                }
                _ => {}
            },
        );

        Self {
            input,
            query: String::new(),
            root,
            all,
            _sub,
        }
    }

    /// Ranked (path, relative display, score), best first, ties broken
    /// alphabetically so an empty query lists the workspace in a stable
    /// order rather than filesystem order.
    fn matches(&self) -> Vec<(PathBuf, String)> {
        let mut scored: Vec<(PathBuf, String, i64)> = self
            .all
            .iter()
            .filter_map(|path| {
                let rel = path
                    .strip_prefix(&self.root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                let score = fuzzy_score(&self.query, &rel)?;
                Some((path.clone(), rel, score))
            })
            .collect();
        scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        scored.truncate(MAX_RESULTS);
        scored
            .into_iter()
            .map(|(path, rel, _)| (path, rel))
            .collect()
    }

    fn open_first_match(&mut self, cx: &mut Context<Self>) {
        let Some((path, _)) = self.matches().into_iter().next() else {
            return;
        };
        let root = self.root.clone();
        open_as_artifact(path, &root, cx);
        // `PressEnter` fires from a subscription, which has no `Window` of
        // its own (unlike a row's `on_mouse_down`) — reach the active
        // window the same way the Cmd+P action itself does.
        if let Some(handle) = cx.active_window() {
            let _ = handle.update(cx, |_root, window, cx| window.close_dialog(cx));
        }
    }
}

impl Render for QuickOpenView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.root.clone();
        let matches = self.matches();
        let is_empty = matches.is_empty();
        let empty_label = if self.query.is_empty() {
            "This workspace has no files."
        } else {
            "No matches."
        };

        v_flex()
            .size_full()
            .gap_2()
            .child(Input::new(&self.input))
            .child(
                v_flex()
                    .id("quick-open-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .when(is_empty, |this| {
                        this.child(
                            div()
                                .px_3()
                                .py_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(empty_label),
                        )
                    })
                    .children(matches.into_iter().enumerate().map(|(ix, (path, rel))| {
                        let root = root.clone();
                        div()
                            .id(ix)
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().secondary))
                            .child(
                                h_flex().w_full().items_center().gap_2().child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_ellipsis()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_color(cx.theme().foreground)
                                        .child(rel),
                                ),
                            )
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                open_as_artifact(path.clone(), &root, cx);
                                window.close_dialog(cx);
                            })
                    })),
            )
    }
}

/// Static helper that opens the Cmd/Ctrl+P quick-open dialog. Mirrors
/// [`super::SearchConversationsDialog::open`].
pub struct QuickOpenDialog;

impl QuickOpenDialog {
    pub fn open(window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| QuickOpenView::new(window, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("Go to File")
                .w(px(560.))
                .h(px(440.))
                .child(view.clone())
        });
    }
}
