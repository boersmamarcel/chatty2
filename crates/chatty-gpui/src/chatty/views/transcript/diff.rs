use std::path::PathBuf;
use std::rc::Rc;

use chatty_core::models::message_types::ToolCallBlock;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Sizable};
use similar::{ChangeTag, TextDiff};

use super::OpenArtifact;
use super::diff_parse::{parse_unified_diff, split_path};

#[derive(Clone, Debug)]
pub struct WordSpan {
    pub text: String,
    pub added: bool,
}

/// Word-level tint spans via workspace `similar`.
pub fn word_spans(old: &str, new: &str) -> Vec<WordSpan> {
    let diff = TextDiff::from_words(old, new);
    diff.iter_all_changes()
        .filter(|c| c.tag() != ChangeTag::Equal)
        .map(|c| WordSpan {
            text: c.to_string_lossy().to_string(),
            added: c.tag() == ChangeTag::Insert,
        })
        .collect()
}

#[derive(IntoElement)]
pub struct DiffStatRow {
    path: String,
    added: usize,
    removed: usize,
    id: String,
}

impl DiffStatRow {
    pub fn new(
        id: impl Into<String>,
        path: impl Into<String>,
        added: usize,
        removed: usize,
    ) -> Self {
        Self {
            path: path.into(),
            added,
            removed,
            id: id.into(),
        }
    }
}

fn stat_bar(added: usize, removed: usize, cx: &App) -> impl IntoElement {
    let total = (added + removed).max(1);
    let segs = 5_usize;
    let plus_segs = ((added * segs) / total).min(segs);
    div()
        .flex()
        .flex_row()
        .gap(px(2.))
        .children((0..segs).map(move |i| {
            let color = if i < plus_segs {
                cx.theme().green
            } else if removed > 0 {
                cx.theme().red
            } else {
                cx.theme().muted
            };
            div().w(px(7.)).h(px(8.)).rounded_sm().bg(color)
        }))
}

impl RenderOnce for DiffStatRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (dir, base) = split_path(&self.path);
        div()
            .id(ElementId::Name(format!("diff-stat-{}", self.id).into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .min_w_0()
                    .flex_1()
                    .font_family("monospace")
                    .text_xs()
                    .when(!dir.is_empty(), |this| {
                        this.child(div().text_color(cx.theme().muted_foreground).child(dir))
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(base),
                    ),
            )
            .when(self.added > 0, |this| {
                this.child(Tag::success().small().child(format!("+{}", self.added)))
            })
            .when(self.removed > 0, |this| {
                this.child(Tag::danger().small().child(format!("−{}", self.removed)))
            })
            .child(stat_bar(self.added, self.removed, cx))
    }
}

#[derive(Clone)]
struct DiffRow {
    tag: ChangeTag,
    text: String,
    old_no: Option<u32>,
    new_no: Option<u32>,
    tint: Vec<(String, bool)>,
}

#[derive(IntoElement)]
pub struct DiffHunkList {
    id: String,
    path: String,
    hunk: String,
    old: String,
    new: String,
    on_open: Option<OpenArtifact>,
}

impl DiffHunkList {
    pub fn new(id: impl Into<String>, old: impl Into<String>, new: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            path: String::new(),
            hunk: String::new(),
            old: old.into(),
            new: new.into(),
            on_open: None,
        }
    }

    pub fn on_open(mut self, f: impl Fn(PathBuf, String, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(f));
        self
    }

    pub fn from_tool(id: impl Into<String>, tool: &ToolCallBlock) -> Self {
        let output = tool.output.as_deref().unwrap_or("");
        if let Some(parsed) = parse_unified_diff(output) {
            return Self {
                id: id.into(),
                path: parsed.path,
                hunk: parsed.hunk,
                old: parsed.old,
                new: parsed.new,
                on_open: None,
            };
        }
        Self {
            id: id.into(),
            path: String::new(),
            hunk: String::new(),
            old: String::new(),
            new: output.to_string(),
            on_open: None,
        }
    }
}

impl RenderOnce for DiffHunkList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ops = TextDiff::from_lines(&self.old, &self.new);
        let mut old_no = 1_u32;
        let mut new_no = 1_u32;
        let changes: Vec<_> = ops.iter_all_changes().collect();
        let mut rows = Vec::new();
        let mut i = 0;
        while i < changes.len() {
            let c = &changes[i];
            let text = c.to_string_lossy().trim_end_matches('\n').to_string();
            match c.tag() {
                ChangeTag::Delete
                    if changes
                        .get(i + 1)
                        .is_some_and(|n| n.tag() == ChangeTag::Insert) =>
                {
                    let next = changes[i + 1]
                        .to_string_lossy()
                        .trim_end_matches('\n')
                        .to_string();
                    let words = word_spans(&text, &next);
                    rows.push(DiffRow {
                        tag: ChangeTag::Delete,
                        text,
                        old_no: Some(old_no),
                        new_no: None,
                        tint: words
                            .iter()
                            .filter(|w| !w.added)
                            .map(|w| (w.text.clone(), false))
                            .collect(),
                    });
                    rows.push(DiffRow {
                        tag: ChangeTag::Insert,
                        text: next,
                        old_no: None,
                        new_no: Some(new_no),
                        tint: words
                            .into_iter()
                            .filter(|w| w.added)
                            .map(|w| (w.text, true))
                            .collect(),
                    });
                    old_no += 1;
                    new_no += 1;
                    i += 2;
                }
                ChangeTag::Insert => {
                    rows.push(DiffRow {
                        tag: ChangeTag::Insert,
                        text,
                        old_no: None,
                        new_no: Some(new_no),
                        tint: Vec::new(),
                    });
                    new_no += 1;
                    i += 1;
                }
                ChangeTag::Delete => {
                    rows.push(DiffRow {
                        tag: ChangeTag::Delete,
                        text,
                        old_no: Some(old_no),
                        new_no: None,
                        tint: Vec::new(),
                    });
                    old_no += 1;
                    i += 1;
                }
                ChangeTag::Equal => {
                    rows.push(DiffRow {
                        tag: ChangeTag::Equal,
                        text,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        tint: Vec::new(),
                    });
                    old_no += 1;
                    new_no += 1;
                    i += 1;
                }
            }
        }

        let added = rows.iter().filter(|r| r.tag == ChangeTag::Insert).count();
        let removed = rows.iter().filter(|r| r.tag == ChangeTag::Delete).count();
        let unchanged = rows.iter().filter(|r| r.tag == ChangeTag::Equal).count();
        let show_fold = unchanged > 3;
        let header_path = if self.path.is_empty() {
            "1 file changed".to_string()
        } else {
            self.path.clone()
        };

        let open_path = PathBuf::from(self.path.clone());
        let open_source = self.new.clone();
        let on_open = self.on_open.clone();

        div()
            .id(ElementId::Name(format!("diff-hunks-{}", self.id).into()))
            .w_full()
            .max_h(px(400.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .rounded_xl()
            .bg(cx.theme().group_box)
            .py_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .child(div().flex_1().min_w_0().child(DiffStatRow::new(
                        self.id.clone(),
                        header_path,
                        added,
                        removed,
                    )))
                    .when_some(on_open, |this, cb| {
                        this.child(
                            Button::new(ElementId::Name(format!("open-diff-{}", self.id).into()))
                                .ghost()
                                .small()
                                .label("Open in panel")
                                .on_click(move |_, _, cx| {
                                    cb(open_path.clone(), open_source.clone(), cx);
                                }),
                        )
                    }),
            )
            .when(!self.hunk.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .font_family("monospace")
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(self.hunk.clone()),
                )
            })
            .children(
                rows.into_iter()
                    .enumerate()
                    .filter(|(_, row)| !(show_fold && row.tag == ChangeTag::Equal))
                    .map(|(i, row)| render_diff_line(&self.id, i, row, cx)),
            )
            .when(show_fold, |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("⋯ {unchanged} unchanged lines")),
                )
            })
    }
}

fn render_diff_line(id: &str, i: usize, row: DiffRow, cx: &App) -> AnyElement {
    let (bg, fg, prefix) = match row.tag {
        ChangeTag::Insert => (cx.theme().green.opacity(0.14), cx.theme().green, "+"),
        ChangeTag::Delete => (cx.theme().red.opacity(0.14), cx.theme().red, "-"),
        ChangeTag::Equal => (gpui::transparent_black(), cx.theme().muted_foreground, " "),
    };
    let gutter = format!(
        "{:>3} {:>3}",
        row.old_no.map(|n| n.to_string()).unwrap_or_default(),
        row.new_no.map(|n| n.to_string()).unwrap_or_default()
    );
    div()
        .id(ElementId::Name(format!("diff-row-{id}-{i}").into()))
        .flex()
        .flex_row()
        .w_full()
        .bg(bg)
        .font_family("monospace")
        .text_xs()
        .px_1()
        .child(
            div()
                .w(px(52.))
                .text_color(cx.theme().muted_foreground)
                .child(gutter),
        )
        .child(div().w(px(14.)).text_color(fg).child(prefix))
        .child(paint_tinted_line(&row.text, &row.tint, fg, cx))
        .into_any_element()
}

fn paint_tinted_line(text: &str, tint: &[(String, bool)], fg: Hsla, cx: &App) -> AnyElement {
    if tint.is_empty() {
        return div()
            .flex_1()
            .text_color(fg)
            .child(text.to_string())
            .into_any_element();
    }
    let mut rest = text;
    let mut line = div().flex().flex_row().flex_1().flex_wrap();
    for (span, added) in tint {
        if let Some(idx) = rest.find(span.as_str()) {
            if idx > 0 {
                line = line.child(div().text_color(fg).child(rest[..idx].to_string()));
            }
            let wash = if *added {
                cx.theme().green.opacity(0.4)
            } else {
                cx.theme().red.opacity(0.4)
            };
            line = line.child(div().bg(wash).text_color(fg).child(span.clone()));
            rest = &rest[idx + span.len()..];
        }
    }
    if !rest.is_empty() {
        line = line.child(div().text_color(fg).child(rest.to_string()));
    }
    line.into_any_element()
}
