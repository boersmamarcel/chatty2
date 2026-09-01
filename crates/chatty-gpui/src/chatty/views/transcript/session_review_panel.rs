use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::Tag;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, VirtualListScrollHandle, v_virtual_list,
};

use super::artifact_kind::artifact_display_path;
use super::artifact_view::ArtifactView;
use super::diff_parse::split_path;
use crate::chatty::views::diff_view_component::{
    CachedDiffView, DiffRenderCache, REVIEW_MAX_LINES, REVIEW_PREVIEW_LINES, diff_line_stats_fast,
    prepare_diff_cache,
};

const REVIEW_HEADER_HEIGHT: f32 = 36.0;
const REVIEW_DIFF_LINE_HEIGHT: f32 = 20.0;
const REVIEW_DIFF_PADDING: f32 = 20.0;
const REVIEW_EXPANDER_HEIGHT: f32 = 28.0;

/// Directory (muted, may truncate) immediately followed by basename.
fn review_path_header(dir: &str, base: &str, muted: Hsla, foreground: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .min_w_0()
        .flex_1()
        .overflow_hidden()
        .font_family("monospace")
        .text_xs()
        .when(!dir.is_empty(), |this| {
            this.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .truncate()
                    .text_color(muted)
                    .child(dir.to_string()),
            )
        })
        .child(
            div()
                .flex_shrink_0()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(foreground)
                .child(base.to_string()),
        )
}

/// Cached diff body for one review file. Separate entity so expanding/toggling
/// diff lines does not rebuild sibling file sections.
pub struct ReviewDiffPane {
    cache: DiffRenderCache,
    fully_expanded: bool,
    file_ix: usize,
    artifact_view: Entity<ArtifactView>,
}

impl ReviewDiffPane {
    fn expand(&mut self, cx: &mut Context<Self>) {
        if self.fully_expanded {
            return;
        }
        self.fully_expanded = true;
        cx.notify();
        self.artifact_view
            .update(cx, |view, cx| view.bump_review_layout(cx));
    }

    pub fn estimated_height(&self) -> f32 {
        let total_items = self.cache.items.len();
        if total_items == 0 {
            return REVIEW_DIFF_PADDING + 24.0;
        }
        let visible = if self.fully_expanded {
            total_items.min(REVIEW_MAX_LINES)
        } else {
            total_items.min(REVIEW_PREVIEW_LINES)
        };
        let mut height = REVIEW_DIFF_PADDING + visible as f32 * REVIEW_DIFF_LINE_HEIGHT;
        if !self.fully_expanded && total_items > REVIEW_PREVIEW_LINES {
            height += REVIEW_EXPANDER_HEIGHT;
        } else if self.fully_expanded && total_items > REVIEW_MAX_LINES {
            height += 24.0;
        }
        height
    }
}

impl Render for ReviewDiffPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().px_2().pb_3().child(
            CachedDiffView::new(self.cache.clone(), self.file_ix, 0, self.fully_expanded)
                .review_mode()
                .on_expand(cx.listener(|this, _, _, cx| {
                    this.expand(cx);
                })),
        )
    }
}

/// One foldable file row in session review.
pub struct ReviewFileSection {
    path: PathBuf,
    dir: String,
    base: String,
    old_content: String,
    new_content: String,
    added: usize,
    removed: usize,
    collapsed: bool,
    file_ix: usize,
    diff_pane: Option<Entity<ReviewDiffPane>>,
    artifact_view: Entity<ArtifactView>,
}

impl ReviewFileSection {
    pub fn new(
        path: PathBuf,
        new_content: String,
        old_content: Option<String>,
        file_ix: usize,
        collapsed: bool,
        workspace_root: Option<String>,
        artifact_view: Entity<ArtifactView>,
    ) -> Self {
        let workspace = workspace_root.as_deref().map(std::path::Path::new);
        let path_display = artifact_display_path(&path, workspace);
        let (dir, base) = split_path(&path_display);
        let old_content = old_content.unwrap_or_default();
        let (added, removed) = diff_line_stats_fast(&old_content, &new_content);
        Self {
            path,
            dir,
            base,
            old_content,
            new_content,
            added,
            removed,
            collapsed,
            file_ix,
            diff_pane: None,
            artifact_view,
        }
    }

    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    pub fn estimated_height(&self, cx: &App) -> f32 {
        if self.collapsed {
            return REVIEW_HEADER_HEIGHT;
        }
        REVIEW_HEADER_HEIGHT
            + self
                .diff_pane
                .as_ref()
                .map(|pane| pane.read(cx).estimated_height())
                .unwrap_or(
                    REVIEW_DIFF_PADDING + REVIEW_PREVIEW_LINES as f32 * REVIEW_DIFF_LINE_HEIGHT,
                )
    }

    fn ensure_diff_pane(&mut self, cx: &mut Context<Self>) {
        if self.diff_pane.is_some() {
            return;
        }
        let cache = prepare_diff_cache(&self.old_content, &self.new_content);
        let file_ix = self.file_ix;
        let artifact_view = self.artifact_view.clone();
        self.diff_pane = Some(cx.new(|_| ReviewDiffPane {
            cache,
            fully_expanded: false,
            file_ix,
            artifact_view,
        }));
    }

    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        if !self.collapsed {
            self.ensure_diff_pane(cx);
        }
        cx.notify();
        self.artifact_view
            .update(cx, |view, cx| view.bump_review_layout(cx));
    }
}

impl Render for ReviewFileSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let chevron = if self.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        let artifact_view = self.artifact_view.clone();
        let path = self.path.clone();
        let new_content = self.new_content.clone();
        let old_snapshot = if self.old_content.is_empty() {
            None
        } else {
            Some(self.old_content.clone())
        };
        let file_ix = self.file_ix;
        let added = self.added;
        let removed = self.removed;
        let dir = self.dir.clone();
        let base = self.base.clone();
        let diff_pane = self.diff_pane.clone();
        let foreground = cx.theme().foreground;

        div()
            .flex()
            .flex_col()
            .w_full()
            .overflow_hidden()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .id(("review-file-header", file_ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .h(px(REVIEW_HEADER_HEIGHT))
                    .child({
                        let toggle = div()
                            .id(("review-file-toggle", file_ix))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.toggle_collapsed(cx);
                                }),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .child(Icon::new(chevron).size_3().text_color(muted)),
                            )
                            .child(review_path_header(&dir, &base, muted, foreground));
                        toggle
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .when(added > 0, |this| {
                                this.child(Tag::success().small().child(format!("+{added}")))
                            })
                            .when(removed > 0, |this| {
                                this.child(Tag::danger().small().child(format!("−{removed}")))
                            })
                            .child(
                                Button::new(("review-open", file_ix))
                                    .ghost()
                                    .small()
                                    .label("Open")
                                    .on_click({
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            artifact_view.update(cx, |view, cx| {
                                                view.open_single_from_review(
                                                    path.clone(),
                                                    new_content.clone(),
                                                    old_snapshot.clone(),
                                                    cx,
                                                );
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            .when(!self.collapsed, |this| this.children(diff_pane.into_iter()))
    }
}

#[derive(IntoElement)]
pub struct SessionReviewPanel {
    file_count: usize,
    total_added: usize,
    total_removed: usize,
    layout_gen: u64,
    artifact_view: Entity<ArtifactView>,
    scroll: VirtualListScrollHandle,
}

impl SessionReviewPanel {
    pub fn new(
        file_count: usize,
        total_added: usize,
        total_removed: usize,
        layout_gen: u64,
        artifact_view: Entity<ArtifactView>,
        scroll: VirtualListScrollHandle,
    ) -> Self {
        Self {
            file_count,
            total_added,
            total_removed,
            layout_gen,
            artifact_view,
            scroll,
        }
    }
}

impl RenderOnce for SessionReviewPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let sizes = Rc::new(self.artifact_view.read(cx).review_section_sizes(cx));
        let layout_gen = self.layout_gen;
        let artifact_view = self.artifact_view.clone();
        let scroll = self.scroll.clone();

        div()
            .id(("session-review-panel", layout_gen))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .h_full()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{} files changed", self.file_count)),
                    )
                    .when(self.total_added > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(gpui::green())
                                .child(format!("+{}", self.total_added)),
                        )
                    })
                    .when(self.total_removed > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().ring)
                                .child(format!("−{}", self.total_removed)),
                        )
                    }),
            )
            .child(
                div()
                    .id("session-review-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .vertical_scrollbar(&scroll)
                    .child(
                        v_virtual_list(
                            artifact_view.clone(),
                            ("session-review-files", layout_gen),
                            sizes,
                            |view, range: Range<usize>, window, cx| {
                                view.render_review_sections(range, window, cx)
                            },
                        )
                        .track_scroll(&scroll)
                        .flex_1(),
                    ),
            )
    }
}
