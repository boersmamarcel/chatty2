use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use similar::{ChangeTag, TextDiff};

/// Callback type for mouse-down events (expand diff, etc.).
type MouseDownCallback = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

/// Maximum combined content size (bytes) before falling back to summary-only.
const MAX_CONTENT_SIZE: usize = 100_000;

/// Number of diff lines shown before the "Show more" expander kicks in.
const PREVIEW_LINES: usize = 10;

/// Number of equal (context) lines to show around each change hunk.
const CONTEXT_LINES: usize = 3;

/// A single diff line with its change tag and text.
struct DiffLine {
    tag: ChangeTag,
    text: String,
}

/// A renderable item in the collapsed diff view.
enum DiffItem {
    Line(DiffLine),
    CollapsedEqual(usize), // number of hidden equal lines
}

/// Visual diff view for `apply_diff` tool calls.
///
/// Shows line-by-line additions (green) and deletions (red) inline within the
/// tool call accordion. Long runs of unchanged lines are collapsed with a
/// separator. Large diffs are preview-capped with an expand button.
#[derive(IntoElement)]
pub struct DiffViewComponent {
    old_content: String,
    new_content: String,
    file_path: String,
    message_index: usize,
    tool_index: usize,
    is_fully_expanded: bool,
    on_expand: Option<MouseDownCallback>,
}

impl DiffViewComponent {
    pub fn new(
        old_content: String,
        new_content: String,
        file_path: String,
        message_index: usize,
        tool_index: usize,
        is_fully_expanded: bool,
    ) -> Self {
        Self {
            old_content,
            new_content,
            file_path,
            message_index,
            tool_index,
            is_fully_expanded,
            on_expand: None,
        }
    }

    pub fn on_expand(
        mut self,
        cb: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_expand = Some(Box::new(cb));
        self
    }
}

/// Build the list of DiffItems, collapsing long runs of equal lines.
fn build_diff_items(old: &str, new: &str) -> (Vec<DiffItem>, usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let raw_lines: Vec<DiffLine> = diff
        .iter_all_changes()
        .map(|change| DiffLine {
            tag: change.tag(),
            text: change.to_string_lossy().to_string(),
        })
        .collect();

    let mut insertions: usize = 0;
    let mut deletions: usize = 0;
    for line in &raw_lines {
        match line.tag {
            ChangeTag::Insert => insertions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }

    // Mark which lines are "near" a change (within CONTEXT_LINES)
    let len = raw_lines.len();
    let mut near_change = vec![false; len];
    for (i, line) in raw_lines.iter().enumerate() {
        if line.tag != ChangeTag::Equal {
            let start = i.saturating_sub(CONTEXT_LINES);
            let end = (i + CONTEXT_LINES + 1).min(len);
            for flag in near_change[start..end].iter_mut() {
                *flag = true;
            }
        }
    }

    // Build items, collapsing runs of equal lines that are far from changes
    let mut items = Vec::new();
    let mut collapse_count: usize = 0;

    for (i, line) in raw_lines.into_iter().enumerate() {
        if line.tag == ChangeTag::Equal && !near_change[i] {
            collapse_count += 1;
        } else {
            if collapse_count > 0 {
                items.push(DiffItem::CollapsedEqual(collapse_count));
                collapse_count = 0;
            }
            items.push(DiffItem::Line(line));
        }
    }
    if collapse_count > 0 {
        items.push(DiffItem::CollapsedEqual(collapse_count));
    }

    (items, insertions, deletions)
}

impl RenderOnce for DiffViewComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border_color = cx.theme().border;
        let muted_bg = cx.theme().muted;
        let muted_text = cx.theme().muted_foreground;
        let _text_color = cx.theme().foreground;

        let total_size = self.old_content.len() + self.new_content.len();

        // Header: file path + stats
        let (items, insertions, deletions) = if total_size <= MAX_CONTENT_SIZE {
            build_diff_items(&self.old_content, &self.new_content)
        } else {
            (Vec::new(), 0, 0)
        };

        let stats_text = format!("+{insertions} \u{2212}{deletions}");

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(muted_text)
                    .child(self.file_path.clone()),
            )
            .child(
                div().text_xs().px_1().rounded_sm().bg(muted_bg).child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(
                            div()
                                .text_color(gpui::green())
                                .child(format!("+{insertions}")),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().ring)
                                .child(format!("\u{2212}{deletions}")),
                        ),
                ),
            );

        // If content is too large, show summary only
        if total_size > MAX_CONTENT_SIZE {
            return div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(border_color)
                .rounded_md()
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(muted_text)
                        .font_family("monospace")
                        .child(format!("Diff too large to display ({} bytes)", total_size)),
                )
                .into_any_element();
        }

        // Count renderable lines (each DiffItem::Line counts as 1, CollapsedEqual as 1)
        let total_items = items.len();
        let should_truncate = !self.is_fully_expanded && total_items > PREVIEW_LINES;
        let visible_count = if should_truncate {
            PREVIEW_LINES
        } else {
            total_items
        };

        let insert_bg = gpui::green().opacity(0.12);
        let delete_bg = cx.theme().ring.opacity(0.12);
        let insert_text = gpui::green();
        let delete_text = cx.theme().ring;

        // Render visible diff lines
        let line_elements: Vec<AnyElement> = items
            .iter()
            .take(visible_count)
            .enumerate()
            .map(|(i, item)| match item {
                DiffItem::Line(line) => {
                    let (bg, prefix, line_color) = match line.tag {
                        ChangeTag::Insert => (insert_bg, "+", insert_text),
                        ChangeTag::Delete => (delete_bg, "-", delete_text),
                        ChangeTag::Equal => (gpui::transparent_black(), " ", muted_text),
                    };

                    // Strip trailing newline for display
                    let display_text = line.text.trim_end_matches('\n').to_string();

                    div()
                        .id(ElementId::Name(
                            format!(
                                "diff-line-{}-{}-{}",
                                self.message_index, self.tool_index, i
                            )
                            .into(),
                        ))
                        .flex()
                        .flex_row()
                        .w_full()
                        .bg(bg)
                        .font_family("monospace")
                        .text_xs()
                        .line_height(relative(1.6))
                        .child(
                            div()
                                .w(px(16.0))
                                .flex_shrink_0()
                                .text_color(line_color)
                                .text_center()
                                .child(prefix),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(line_color)
                                .child(display_text),
                        )
                        .into_any_element()
                }
                DiffItem::CollapsedEqual(count) => div()
                    .id(ElementId::Name(
                        format!(
                            "diff-collapse-{}-{}-{}",
                            self.message_index, self.tool_index, i
                        )
                        .into(),
                    ))
                    .w_full()
                    .text_center()
                    .text_xs()
                    .text_color(muted_text)
                    .py(px(2.0))
                    .font_family("monospace")
                    .child(format!(
                        "\u{00b7}\u{00b7}\u{00b7} {count} unchanged line{} \u{00b7}\u{00b7}\u{00b7}",
                        if *count == 1 { "" } else { "s" }
                    ))
                    .into_any_element(),
            })
            .collect();

        let mut container = div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_1()
                    .py_1()
                    .children(line_elements),
            );

        // "Show N more lines" expander
        if should_truncate {
            let remaining = total_items - PREVIEW_LINES;
            let expander = div()
                .id(ElementId::Name(
                    format!("diff-expand-{}-{}", self.message_index, self.tool_index).into(),
                ))
                .w_full()
                .text_center()
                .cursor_pointer()
                .py_1()
                .bg(muted_bg.opacity(0.5))
                .border_t_1()
                .border_color(border_color)
                .text_xs()
                .text_color(cx.theme().primary)
                .font_weight(FontWeight::MEDIUM)
                .child(format!(
                    "\u{25b6} Show {remaining} more line{}",
                    if remaining == 1 { "" } else { "s" }
                ))
                .when_some(
                    self.on_expand,
                    |this: Stateful<Div>, cb: MouseDownCallback| {
                        this.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                            cb(event, window, cx);
                        })
                    },
                );

            container = container.child(expander);
        }

        // Also show the raw stats line from the tool output
        container = container.child(
            div()
                .px_2()
                .py(px(2.0))
                .border_t_1()
                .border_color(border_color)
                .text_xs()
                .text_color(muted_text)
                .font_family("monospace")
                .child(stats_text),
        );

        container.into_any_element()
    }
}
