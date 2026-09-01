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

/// Session review: smaller preview before "Show more".
pub const REVIEW_PREVIEW_LINES: usize = 8;

/// Session review: hard cap on rendered diff rows even after "Show more".
pub const REVIEW_MAX_LINES: usize = 32;

const REVIEW_DIFF_LINE_HEIGHT: f32 = 20.0;
const REVIEW_DIFF_PADDING: f32 = 20.0;
const REVIEW_EXPANDER_HEIGHT: f32 = 28.0;

/// Height reserved for one review diff block (must match rendered layout).
pub fn review_diff_estimated_height(cache: &DiffRenderCache, fully_expanded: bool) -> f32 {
    let total_items = cache.items.len();
    if total_items == 0 {
        return REVIEW_DIFF_PADDING + 24.0;
    }
    let visible = if fully_expanded {
        total_items.min(REVIEW_MAX_LINES)
    } else {
        total_items.min(REVIEW_PREVIEW_LINES)
    };
    let mut height = REVIEW_DIFF_PADDING + visible as f32 * REVIEW_DIFF_LINE_HEIGHT;
    if !fully_expanded && total_items > REVIEW_PREVIEW_LINES {
        height += REVIEW_EXPANDER_HEIGHT;
    } else if fully_expanded && total_items > REVIEW_MAX_LINES {
        height += 24.0;
    }
    height
}

/// Number of equal (context) lines to show around each change hunk.
const CONTEXT_LINES: usize = 3;

/// A renderable item in the collapsed diff view.
#[derive(Clone)]
pub enum DiffDisplayItem {
    Line { tag: ChangeTag, text: String },
    CollapsedEqual(usize),
}

/// Precomputed diff rows for reuse across renders (session review).
#[derive(Clone)]
pub struct DiffRenderCache {
    pub items: Vec<DiffDisplayItem>,
    pub insertions: usize,
    pub deletions: usize,
}

/// A single diff line with its change tag and text.
struct DiffLine {
    tag: ChangeTag,
    text: String,
}

/// Internal renderable item (converted to [`DiffDisplayItem`] for cache).
enum DiffItem {
    Line(DiffLine),
    CollapsedEqual(usize),
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
    show_header: bool,
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
            show_header: true,
            on_expand: None,
        }
    }

    /// Hide the built-in path/stats header (session review supplies its own).
    pub fn without_header(mut self) -> Self {
        self.show_header = false;
        self
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

fn items_to_cache(items: Vec<DiffItem>, insertions: usize, deletions: usize) -> DiffRenderCache {
    DiffRenderCache {
        items: items
            .into_iter()
            .map(|item| match item {
                DiffItem::Line(line) => DiffDisplayItem::Line {
                    tag: line.tag,
                    text: line.text,
                },
                DiffItem::CollapsedEqual(n) => DiffDisplayItem::CollapsedEqual(n),
            })
            .collect(),
        insertions,
        deletions,
    }
}

/// Build diff display data once; reuse via [`CachedDiffView`].
pub fn prepare_diff_cache(old: &str, new: &str) -> DiffRenderCache {
    let total_size = old.len() + new.len();
    if total_size > MAX_CONTENT_SIZE {
        return DiffRenderCache {
            items: Vec::new(),
            insertions: 0,
            deletions: 0,
        };
    }
    let (items, insertions, deletions) = build_diff_items(old, new);
    items_to_cache(items, insertions, deletions)
}

/// Count +/- lines without building the display list.
pub fn diff_line_stats_fast(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut insertions = 0_usize;
    let mut deletions = 0_usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => insertions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    (insertions, deletions)
}

/// Line-level insertion/deletion counts for a text diff.
pub fn diff_line_stats(old: &str, new: &str) -> (usize, usize) {
    diff_line_stats_fast(old, new)
}

#[derive(IntoElement)]
pub struct CachedDiffView {
    cache: DiffRenderCache,
    message_index: usize,
    tool_index: usize,
    is_fully_expanded: bool,
    review_mode: bool,
    on_expand: Option<MouseDownCallback>,
}

impl CachedDiffView {
    pub fn new(
        cache: DiffRenderCache,
        message_index: usize,
        tool_index: usize,
        is_fully_expanded: bool,
    ) -> Self {
        Self {
            cache,
            message_index,
            tool_index,
            is_fully_expanded,
            review_mode: false,
            on_expand: None,
        }
    }

    pub fn review_mode(mut self) -> Self {
        self.review_mode = true;
        self
    }

    pub fn on_expand(
        mut self,
        cb: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_expand = Some(Box::new(cb));
        self
    }
}

impl RenderOnce for CachedDiffView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        render_diff_from_cache(
            &self.cache,
            self.message_index,
            self.tool_index,
            self.is_fully_expanded,
            self.review_mode,
            false,
            None,
            None,
            self.on_expand,
            cx,
        )
    }
}

fn render_diff_from_cache(
    cache: &DiffRenderCache,
    message_index: usize,
    tool_index: usize,
    is_fully_expanded: bool,
    review_mode: bool,
    show_header: bool,
    file_label: Option<&str>,
    too_large_bytes: Option<usize>,
    on_expand: Option<MouseDownCallback>,
    cx: &App,
) -> AnyElement {
    let border_color = cx.theme().border;
    let muted_bg = cx.theme().muted;
    let muted_text = cx.theme().muted_foreground;

    let insertions = cache.insertions;
    let deletions = cache.deletions;
    let stats_text = format!("+{insertions} \u{2212}{deletions}");
    let total_items = cache.items.len();

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
                .child(file_label.unwrap_or("diff").to_string()),
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

    if let Some(bytes) = too_large_bytes {
        let mut container = div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .overflow_hidden();
        if show_header {
            container = container.child(header);
        }
        return container
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted_text)
                    .font_family("monospace")
                    .child(format!("Diff too large to display ({bytes} bytes)")),
            )
            .into_any_element();
    }

    if cache.items.is_empty() && (insertions + deletions) == 0 {
        let mut container = div()
            .flex()
            .flex_col()
            .when(show_header, |this| {
                this.border_1()
                    .border_color(border_color)
                    .rounded_md()
                    .overflow_hidden()
            })
            .when(!show_header, |this| this.w_full());
        if show_header {
            container = container.child(header);
        }
        return container
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted_text)
                    .font_family("monospace")
                    .child("Diff too large to display inline"),
            )
            .into_any_element();
    }

    let preview_cap = if review_mode {
        REVIEW_PREVIEW_LINES
    } else {
        PREVIEW_LINES
    };
    let max_cap = if review_mode {
        REVIEW_MAX_LINES
    } else {
        usize::MAX
    };

    let should_truncate = !is_fully_expanded && total_items > preview_cap;
    let mut visible_count = if should_truncate {
        preview_cap
    } else {
        total_items
    };
    if review_mode {
        visible_count = visible_count.min(max_cap);
    }
    let capped_in_review = review_mode && is_fully_expanded && total_items > max_cap;

    let insert_bg = gpui::green().opacity(0.12);
    let delete_bg = cx.theme().ring.opacity(0.12);
    let insert_text = gpui::green();
    let delete_text = cx.theme().ring;

    let line_elements: Vec<AnyElement> = cache
        .items
        .iter()
        .take(visible_count)
        .enumerate()
        .map(|(i, item)| match item {
            DiffDisplayItem::Line { tag, text } => {
                let (bg, prefix, line_color) = match tag {
                    ChangeTag::Insert => (insert_bg, "+", insert_text),
                    ChangeTag::Delete => (delete_bg, "-", delete_text),
                    ChangeTag::Equal => (gpui::transparent_black(), " ", muted_text),
                };
                let display_text = text.trim_end_matches('\n').to_string();
                let mut line = div()
                    .id(ElementId::Name(
                        format!("diff-line-{message_index}-{tool_index}-{i}").into(),
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
                            .when(review_mode, |this| this.overflow_hidden().truncate())
                            .child(display_text),
                    );
                if review_mode {
                    line = line.h(px(REVIEW_DIFF_LINE_HEIGHT)).overflow_hidden();
                }
                line.into_any_element()
            }
            DiffDisplayItem::CollapsedEqual(count) => div()
                .id(ElementId::Name(
                    format!("diff-collapse-{message_index}-{tool_index}-{i}").into(),
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
        .when(show_header, |this| {
            this.border_1()
                .border_color(border_color)
                .rounded_md()
                .overflow_hidden()
        })
        .when(!show_header, |this| this.w_full());
    if show_header {
        container = container.child(header);
    }
    container = container.child(
        div()
            .flex()
            .flex_col()
            .px_1()
            .py_1()
            .children(line_elements),
    );

    if should_truncate {
        let remaining = total_items.saturating_sub(preview_cap);
        let mut expander = div()
            .id(ElementId::Name(
                format!("diff-expand-{message_index}-{tool_index}").into(),
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
            ));
        if let Some(cb) = on_expand {
            expander = expander.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                cb(event, window, cx);
            });
        }
        container = container.child(expander);
    } else if capped_in_review {
        container = container.child(
            div()
                .w_full()
                .text_center()
                .py_1()
                .text_xs()
                .text_color(muted_text)
                .child(format!(
                    "Showing first {REVIEW_MAX_LINES} of {total_items} diff rows — use Open for the full file"
                )),
        );
    }

    if show_header {
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
    }

    container.into_any_element()
}

impl RenderOnce for DiffViewComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let total_size = self.old_content.len() + self.new_content.len();
        let too_large = (total_size > MAX_CONTENT_SIZE).then_some(total_size);
        let cache = if too_large.is_some() {
            DiffRenderCache {
                items: Vec::new(),
                insertions: 0,
                deletions: 0,
            }
        } else {
            prepare_diff_cache(&self.old_content, &self.new_content)
        };
        render_diff_from_cache(
            &cache,
            self.message_index,
            self.tool_index,
            self.is_fully_expanded,
            false,
            self.show_header,
            Some(&self.file_path),
            too_large,
            self.on_expand,
            cx,
        )
    }
}
