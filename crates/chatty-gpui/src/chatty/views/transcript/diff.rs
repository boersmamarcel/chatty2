use gpui::*;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Sizable};
use similar::{ChangeTag, TextDiff};

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

impl RenderOnce for DiffStatRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let total = (self.added + self.removed).max(1);
        let segs = 5_usize;
        let plus_segs = ((self.added * segs) / total).min(segs);
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
                    .text_xs()
                    .font_family("monospace")
                    .text_color(cx.theme().muted_foreground)
                    .child(self.path),
            )
            .child(Tag::success().small().child(format!("+{}", self.added)))
            .child(Tag::danger().small().child(format!("−{}", self.removed)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(2.))
                    .children((0..segs).map(|i| {
                        let color = if i < plus_segs {
                            cx.theme().green
                        } else {
                            cx.theme().red
                        };
                        div().w(px(6.)).h(px(8.)).rounded_sm().bg(color)
                    })),
            )
    }
}

#[derive(Clone)]
struct DiffRow {
    tag: ChangeTag,
    text: String,
}

#[derive(IntoElement)]
pub struct DiffHunkList {
    id: String,
    old: String,
    new: String,
}

impl DiffHunkList {
    pub fn new(id: impl Into<String>, old: impl Into<String>, new: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            old: old.into(),
            new: new.into(),
        }
    }
}

impl RenderOnce for DiffHunkList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let rows: Vec<DiffRow> = TextDiff::from_lines(&self.old, &self.new)
            .iter_all_changes()
            .map(|c| DiffRow {
                tag: c.tag(),
                text: c.to_string_lossy().trim_end_matches('\n').to_string(),
            })
            .collect();
        let added = rows.iter().filter(|r| r.tag == ChangeTag::Insert).count();
        let removed = rows.iter().filter(|r| r.tag == ChangeTag::Delete).count();

        div()
            .id(ElementId::Name(format!("diff-hunks-{}", self.id).into()))
            .max_h(px(400.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(DiffStatRow::new(self.id.clone(), "", added, removed))
            .children(rows.into_iter().enumerate().map(|(i, row)| {
                let (bg, fg, prefix) = match row.tag {
                    ChangeTag::Insert => (cx.theme().green.opacity(0.12), cx.theme().green, "+"),
                    ChangeTag::Delete => (cx.theme().red.opacity(0.12), cx.theme().red, "-"),
                    ChangeTag::Equal => {
                        (gpui::transparent_black(), cx.theme().muted_foreground, " ")
                    }
                };
                div()
                    .id(ElementId::Name(format!("diff-row-{}-{i}", self.id).into()))
                    .flex()
                    .flex_row()
                    .w_full()
                    .bg(bg)
                    .font_family("monospace")
                    .text_xs()
                    .child(div().w(px(16.)).text_color(fg).child(prefix))
                    .child(div().flex_1().text_color(fg).child(row.text))
            }))
    }
}
