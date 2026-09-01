//! Session- and turn-level file change summaries for the transcript.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use chatty_core::models::message_types::{ToolCallBlock, ToolCallState};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::activity::{ToolKind, classify_tool};
use super::artifact_kind::tool_file_path;
use super::diff::DiffStatRow;
use super::types::{Block, Turn};
use super::verb::diff_stats;

/// One workspace file touched by an edit/write tool in this conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub old: String,
    pub new: String,
    pub added: usize,
    pub removed: usize,
}

impl FileChange {
    pub fn path_display(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

pub fn file_change_from_tool(tool: &ToolCallBlock) -> Option<FileChange> {
    if !matches!(classify_tool(&tool.tool_name), ToolKind::Edit) {
        return None;
    }
    if !matches!(tool.state, ToolCallState::Success) {
        return None;
    }
    let path =
        tool_file_path(&tool.input).or_else(|| tool.output.as_deref().and_then(tool_file_path))?;
    let (old, new) = edit_bodies(&tool.input);
    let (added, removed) = diff_stats(&tool.tool_name, &tool.input, tool.output.as_deref());
    Some(FileChange {
        path,
        old: old.unwrap_or_default(),
        new: new.unwrap_or_default(),
        added: added.unwrap_or(0),
        removed: removed.unwrap_or(0),
    })
}

fn edit_bodies(input: &str) -> (Option<String>, Option<String>) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(input) else {
        return (None, None);
    };
    let old = json
        .get("old_content")
        .or_else(|| json.get("old_string"))
        .or_else(|| json.get("old"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let new = json
        .get("new_content")
        .or_else(|| json.get("new_string"))
        .or_else(|| json.get("new"))
        .or_else(|| json.get("content"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (old, new)
}

/// Merge edits to the same path: first old body, last new body, summed stats.
pub fn merge_file_changes(changes: impl IntoIterator<Item = FileChange>) -> Vec<FileChange> {
    let mut by_path: BTreeMap<PathBuf, FileChange> = BTreeMap::new();
    for change in changes {
        by_path
            .entry(change.path.clone())
            .and_modify(|existing| {
                if existing.old.is_empty() && !change.old.is_empty() {
                    existing.old = change.old.clone();
                }
                if !change.new.is_empty() {
                    existing.new = change.new.clone();
                }
                existing.added += change.added;
                existing.removed += change.removed;
            })
            .or_insert(change);
    }
    by_path.into_values().collect()
}

pub fn collect_file_changes_from_tools(tools: &[ToolCallBlock]) -> Vec<FileChange> {
    merge_file_changes(tools.iter().filter_map(file_change_from_tool))
}

/// Edits already counted on Activity blocks — Diff is the same tool again.
pub fn file_changes_from_turn(turn: &Turn) -> Vec<FileChange> {
    let mut collected = Vec::new();
    for block in &turn.blocks {
        if let Block::Activity { tools, .. } = block {
            collected.extend(tools.iter().filter_map(file_change_from_tool));
        }
    }
    merge_file_changes(collected)
}

pub fn file_changes_height(changes: &[FileChange]) -> f32 {
    if changes.is_empty() {
        0.0
    } else {
        36.0 + changes.len() as f32 * 28.0
    }
}

type SessionAction = Rc<dyn Fn(&mut App)>;
type SessionOpenFile = Rc<dyn Fn(PathBuf, &mut App)>;

/// Compact session banner: "N files changed +X −Y" with Review / Keep all.
#[derive(IntoElement)]
pub struct SessionChangeBar {
    changes: Vec<FileChange>,
    expanded: bool,
    on_toggle: Option<SessionAction>,
    on_review: Option<SessionAction>,
    on_keep_all: Option<SessionAction>,
    on_open_file: Option<SessionOpenFile>,
}

impl SessionChangeBar {
    pub fn new(changes: Vec<FileChange>) -> Self {
        Self {
            changes,
            expanded: false,
            on_toggle: None,
            on_review: None,
            on_keep_all: None,
            on_open_file: None,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn on_toggle(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }

    pub fn on_review(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_review = Some(Rc::new(f));
        self
    }

    pub fn on_keep_all(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_keep_all = Some(Rc::new(f));
        self
    }

    pub fn on_open_file(mut self, f: impl Fn(PathBuf, &mut App) + 'static) -> Self {
        self.on_open_file = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for SessionChangeBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.changes.is_empty() {
            return div().into_any_element();
        }
        let count = self.changes.len();
        let added: usize = self.changes.iter().map(|c| c.added).sum();
        let removed: usize = self.changes.iter().map(|c| c.removed).sum();
        let noun = if count == 1 { "file" } else { "files" };
        let chevron = if self.expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let on_toggle = self.on_toggle;
        let on_open_file = self.on_open_file;
        let expanded = self.expanded;
        let changes = self.changes;

        div()
            .id("session-change-bar")
            .w_full()
            .px_4()
            .pb_2()
            .child(
                div()
                    .id("session-change-bar-inner")
                    .w_full()
                    .flex()
                    .flex_col()
                    .rounded_xl()
                    .bg(cx.theme().group_box)
                    .child(
                        div()
                            .id("session-change-bar-header")
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .child(
                                Button::new("session-change-bar-toggle")
                                    .ghost()
                                    .xsmall()
                                    .on_click(move |_, _, cx| {
                                        if let Some(cb) = &on_toggle {
                                            cb(cx);
                                        }
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                Icon::new(chevron)
                                                    .size_3()
                                                    .text_color(cx.theme().muted_foreground),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(cx.theme().foreground)
                                                    .child(format!("{count} {noun} changed")),
                                            )
                                            .when(added > 0, |this| {
                                                this.child(
                                                    Tag::success()
                                                        .small()
                                                        .child(format!("+{added}")),
                                                )
                                            })
                                            .when(removed > 0, |this| {
                                                this.child(
                                                    Tag::danger()
                                                        .small()
                                                        .child(format!("−{removed}")),
                                                )
                                            }),
                                    ),
                            )
                            .child(div().flex_1())
                            .when_some(self.on_review, |this, cb| {
                                this.child(
                                    Button::new("session-review")
                                        .primary()
                                        .xsmall()
                                        .label("Review")
                                        .on_click(move |_, _, cx| cb(cx)),
                                )
                            })
                            .when_some(self.on_keep_all, |this, cb| {
                                this.child(
                                    Button::new("session-keep-all")
                                        .outline()
                                        .xsmall()
                                        .label("Keep all")
                                        .on_click(move |_, _, cx| cb(cx)),
                                )
                            }),
                    )
                    .when(expanded, |this| {
                        this.child(
                            div()
                                .id("session-change-bar-files")
                                .flex()
                                .flex_col()
                                .gap_1()
                                .px_2()
                                .pb_2()
                                .children(changes.into_iter().map(|change| {
                                    let path = change.path.clone();
                                    let on_open = on_open_file.clone();
                                    let row_id = format!("session-file-{}", change.path_display());
                                    Button::new(ElementId::Name(row_id.into()))
                                        .ghost()
                                        .xsmall()
                                        .w_full()
                                        .on_click(move |_, _, cx| {
                                            if let Some(cb) = &on_open {
                                                cb(path.clone(), cx);
                                            }
                                        })
                                        .child(DiffStatRow::new(
                                            change.path_display(),
                                            change.path_display(),
                                            change.added,
                                            change.removed,
                                        ))
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// Per-turn file list shown when the work fold is open.
#[derive(IntoElement)]
pub struct TurnFileOverview {
    changes: Vec<FileChange>,
}

impl TurnFileOverview {
    pub fn new(changes: Vec<FileChange>) -> Self {
        Self { changes }
    }
}

impl RenderOnce for TurnFileOverview {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.changes.is_empty() {
            return div().into_any_element();
        }
        let count = self.changes.len();
        let added: usize = self.changes.iter().map(|c| c.added).sum();
        let removed: usize = self.changes.iter().map(|c| c.removed).sum();
        let noun = if count == 1 { "file" } else { "files" };

        div()
            .id("turn-file-overview")
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_2()
            .rounded_xl()
            .bg(cx.theme().group_box)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{count} {noun} changed")),
                    )
                    .when(added > 0, |this| {
                        this.child(Tag::success().small().child(format!("+{added}")))
                    })
                    .when(removed > 0, |this| {
                        this.child(Tag::danger().small().child(format!("−{removed}")))
                    }),
            )
            .children(self.changes.into_iter().enumerate().map(|(ix, change)| {
                DiffStatRow::new(
                    format!("turn-file-{ix}"),
                    change.path_display(),
                    change.added,
                    change.removed,
                )
            }))
            .into_any_element()
    }
}
