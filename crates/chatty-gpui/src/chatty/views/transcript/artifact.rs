use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chatty_core::services::pdf_thumbnail::{
    PREVIEW_WIDTH, PdfThumbnailError, pdf_page_count, render_pdf_page,
};
use chatty_core::tools::chart_tool::ChartSpec;
use chatty_core::tools::data_query_tool::{
    FILE_PREVIEW_MAX_ROWS, TablePreview, load_file_table_preview,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::list::ListItem;
use gpui_component::menu::PopupMenuItem;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::tree::{TreeItem, TreeState, tree};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use tracing::warn;

use super::artifact_kind::{
    artifact_language_for_path, is_code_artifact_path, is_image_path, is_markdown_artifact_path,
    is_pdf_path, is_tabular_path, read_artifact_source,
};
use super::artifact_meta::{
    ArtifactVersion, ArtifactViewMode, Heading, ViewAnchor, capture_anchor, card_meta,
    current_version, is_stale, keep_full_when_opening_other, outline_tree, panel_title,
    parse_headings, restore_fraction,
};
use super::diff::DiffHunkList;
use super::run_pin::{RunPin, RunPinKind};
use super::table::render_table_preview_view;
use crate::chatty::views::chart_renderer::render_chart_panel;
use crate::chatty::views::code_block_component::CodeBlockComponent;

/// Inner width used when the dock is at its default 380px. `img` only derives
/// height from aspect ratio when width is an absolute `px()`.
const PDF_PAGE_DISPLAY_WIDTH: f32 = 348.0;
const IMAGE_DISPLAY_WIDTH: f32 = PDF_PAGE_DISPLAY_WIDTH;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ArtifactMode {
    #[default]
    Closed,
    Docked,
    Full,
}

#[derive(Clone, Debug)]
pub enum ArtifactViewEvent {
    Closed,
    ModeChanged,
}

#[derive(Clone, Debug, Default)]
enum PdfPreview {
    #[default]
    Idle,
    Loading {
        page: u32,
    },
    Ready {
        page: u32,
        total: u32,
        image: PathBuf,
    },
    Error(String),
}

#[derive(Clone, Debug, Default)]
enum TabularPreview {
    #[default]
    Idle,
    Loading,
    Ready(TablePreview),
    Error(String),
}

/// One artifact workbench entity: Closed | Docked | Full. Reparent, do not rebuild.
pub struct ArtifactView {
    pub mode: ArtifactMode,
    pub path: Option<PathBuf>,
    pub rendered: String,
    pub source: String,
    pub old: String,
    files: Vec<(PathBuf, String, Option<String>)>,
    tab: usize,
    pdf: PdfPreview,
    tabular: TabularPreview,
    /// Interactive chart from `create_chart` (preferred over a saved PNG when set).
    chart: Option<ChartSpec>,
    workspace_root: Option<String>,
    load_gen: u64,
    tree_state: Entity<TreeState>,
    body_scroll: ScrollHandle,
    version: Option<ArtifactVersion>,
    stale: bool,
    anchor: ViewAnchor,
    pending_scroll: Option<f32>,
    scroll_memory: HashMap<(String, u8), f32>,
}

impl ArtifactView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            mode: ArtifactMode::Closed,
            path: None,
            rendered: String::new(),
            source: String::new(),
            old: String::new(),
            files: Vec::new(),
            tab: 0,
            pdf: PdfPreview::Idle,
            tabular: TabularPreview::Idle,
            chart: None,
            workspace_root: None,
            load_gen: 0,
            tree_state: cx.new(|cx| TreeState::new(cx)),
            body_scroll: ScrollHandle::new(),
            version: None,
            stale: false,
            anchor: ViewAnchor::ScrollFraction(0.0),
            pending_scroll: None,
            scroll_memory: HashMap::new(),
        }
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn open_table(&mut self, preview: TablePreview, cx: &mut Context<Self>) {
        if let chatty_core::tools::data_query_tool::TableSource::File { path } = &preview.source {
            self.path = Some(PathBuf::from(path));
        } else {
            self.path = None;
        }
        self.tabular = TabularPreview::Ready(preview);
        self.pdf = PdfPreview::Idle;
        self.chart = None;
        self.tab = 0;
        self.stale = false;
        self.drop_full_for_new_document(cx);
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
            cx.emit(ArtifactViewEvent::ModeChanged);
        }
        cx.notify();
    }

    pub fn open_chart(&mut self, spec: ChartSpec, cx: &mut Context<Self>) {
        if let Some(saved) = spec.saved_path.as_ref() {
            self.path = Some(PathBuf::from(saved));
        } else {
            self.path = None;
        }
        self.chart = Some(spec);
        self.pdf = PdfPreview::Idle;
        self.tabular = TabularPreview::Idle;
        self.source.clear();
        self.rendered.clear();
        self.old.clear();
        self.tab = 0;
        self.stale = false;
        self.drop_full_for_new_document(cx);
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
            cx.emit(ArtifactViewEvent::ModeChanged);
        }
        cx.notify();
    }

    pub fn open(
        &mut self,
        path: PathBuf,
        source: String,
        old: Option<String>,
        workspace_root: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let switching = self.path.as_ref() != Some(&path);
        if switching {
            self.persist_scroll();
            self.drop_full_for_new_document(cx);
        }
        let old_snapshot = old.clone();
        if !self.files.iter().any(|(existing, _, _)| existing == &path) {
            self.files
                .push((path.clone(), source.clone(), old_snapshot.clone()));
        }
        self.path = Some(path.clone());
        self.workspace_root = workspace_root.clone();
        self.version = Some(current_version(&path));
        self.stale = false;
        if is_pdf_path(&path) {
            self.source.clear();
            self.rendered.clear();
            self.old.clear();
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.tab = 0;
            self.start_pdf_load(0, cx);
        } else if is_tabular_path(&path) {
            self.pdf = PdfPreview::Idle;
            self.chart = None;
            self.source = source.clone();
            self.rendered = source.clone();
            self.old = old_snapshot.clone().unwrap_or_default();
            self.tab = 0;
            self.start_tabular_load(path.clone(), workspace_root, cx);
        } else if is_image_path(&path) {
            self.pdf = PdfPreview::Idle;
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.source.clear();
            self.rendered.clear();
            self.old.clear();
            self.tab = 0;
        } else {
            self.pdf = PdfPreview::Idle;
            self.tabular = TabularPreview::Idle;
            self.chart = None;
            self.source = source.clone();
            self.rendered = source.clone();
            self.old = old_snapshot.clone().unwrap_or_default();
            if switching {
                self.tab = if old_snapshot
                    .as_ref()
                    .is_some_and(|o| !o.is_empty() && o != &source)
                {
                    2
                } else {
                    0
                };
            }
        }
        self.refresh_outline(cx);
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
            cx.emit(ArtifactViewEvent::ModeChanged);
        }
        if let Some(frac) = self
            .path
            .as_ref()
            .and_then(|p| {
                self.scroll_memory
                    .get(&(p.display().to_string(), self.tab as u8))
            })
            .copied()
        {
            self.pending_scroll = Some(frac);
        }
        cx.notify();
    }

    /// Open every session file in the dock, starting on the Diff tab when an
    /// old body is available (review mode). `focus` selects which file to show.
    pub fn open_review(
        &mut self,
        files: Vec<(PathBuf, String, Option<String>)>,
        workspace_root: Option<String>,
        focus: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.files = files;
        let selected = focus
            .and_then(|want| {
                self.files
                    .iter()
                    .find(|(path, _, _)| path == &want)
                    .cloned()
            })
            .or_else(|| self.files.first().cloned());
        let Some((path, source, old)) = selected else {
            return;
        };
        self.open(path, source, old, workspace_root, cx);
        if !self.old.is_empty() && self.old != self.source {
            self.tab = 2;
        }
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: ArtifactMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == ArtifactMode::Closed {
            cx.emit(ArtifactViewEvent::Closed);
        } else {
            cx.emit(ArtifactViewEvent::ModeChanged);
        }
        cx.notify();
    }

    pub fn exit_full_for_approval(&mut self, cx: &mut Context<Self>) {
        if self.mode == ArtifactMode::Full {
            self.set_mode(ArtifactMode::Docked, cx);
        }
    }

    fn drop_full_for_new_document(&mut self, cx: &mut Context<Self>) {
        if self.mode == ArtifactMode::Full && !keep_full_when_opening_other() {
            self.mode = ArtifactMode::Docked;
            cx.emit(ArtifactViewEvent::ModeChanged);
        }
    }

    fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let source = read_artifact_source(&path);
        let old = if self.old.is_empty() {
            None
        } else {
            Some(self.old.clone())
        };
        let workspace = self.workspace_root.clone();
        let keep_tab = self.tab;
        let keep_mode = self.mode;
        self.open(path, source, old, workspace, cx);
        self.tab = keep_tab;
        self.mode = keep_mode;
        self.stale = false;
        cx.notify();
    }

    fn refresh_outline(&mut self, cx: &mut Context<Self>) {
        let items = headings_to_tree_items(&parse_headings(&self.source));
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }

    fn view_mode(&self, has_diff: bool) -> ArtifactViewMode {
        let tab = if !has_diff { self.tab.min(1) } else { self.tab };
        match tab {
            1 => ArtifactViewMode::Source,
            2 => ArtifactViewMode::Diff,
            _ => ArtifactViewMode::Rendered,
        }
    }

    fn persist_scroll(&mut self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let frac = scroll_fraction(&self.body_scroll);
        self.scroll_memory
            .insert((path.display().to_string(), self.tab as u8), frac);
    }

    fn set_tab(&mut self, tab: usize, has_diff: bool, cx: &mut Context<Self>) {
        let headings = parse_headings(&self.source);
        let line_count = self.source.lines().count() as u32;
        let from = self.view_mode(has_diff);
        let frac = scroll_fraction(&self.body_scroll);
        self.anchor = capture_anchor(from, frac, &headings, line_count);
        self.persist_scroll();
        self.tab = if has_diff { tab } else { tab.min(1) };
        let to = self.view_mode(has_diff);
        self.pending_scroll = Some(restore_fraction(self.anchor, to, &headings, line_count));
        cx.notify();
    }

    fn jump_to_line(&mut self, line: u32, has_diff: bool, cx: &mut Context<Self>) {
        let headings = parse_headings(&self.source);
        let line_count = self.source.lines().count() as u32;
        self.anchor = ViewAnchor::SourceLine(line);
        self.pending_scroll = Some(restore_fraction(
            self.anchor,
            self.view_mode(has_diff),
            &headings,
            line_count,
        ));
        cx.notify();
    }

    fn refresh_staleness(&mut self) {
        let Some(path) = self.path.as_ref() else {
            self.stale = false;
            return;
        };
        let Some(loaded) = self.version else {
            self.stale = false;
            return;
        };
        self.stale = is_stale(loaded, current_version(path));
    }

    fn apply_pending_scroll(&mut self) {
        if let Some(frac) = self.pending_scroll.take() {
            apply_scroll_fraction(&self.body_scroll, frac);
        }
    }

    fn start_pdf_load(&mut self, page: u32, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.pdf = PdfPreview::Loading { page };
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || -> Result<_, PdfThumbnailError> {
                let total = pdf_page_count(&path)?;
                let image = render_pdf_page(&path, page, PREVIEW_WIDTH)?;
                Ok((total, image))
            })
            .await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok((total, image))) => {
                        this.pdf = PdfPreview::Ready { page, total, image };
                    }
                    Ok(Err(e)) => this.pdf = PdfPreview::Error(e.to_string()),
                    Err(e) => this.pdf = PdfPreview::Error(e.to_string()),
                }
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "Failed to apply PDF preview"))
            .ok();
        })
        .detach();
    }

    fn start_tabular_load(
        &mut self,
        path: PathBuf,
        workspace_root: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace_root) = workspace_root.filter(|root| !root.is_empty()) else {
            self.tabular = TabularPreview::Error(
                "Set a workspace directory in Settings → Execution to preview tabular files."
                    .into(),
            );
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let load_id = self.load_gen;
        self.tabular = TabularPreview::Loading;
        let file_path = path.to_string_lossy().to_string();
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || {
                load_file_table_preview(&workspace_root, &file_path, FILE_PREVIEW_MAX_ROWS)
            })
            .await;
            this.update(cx, |this, cx| {
                if this.load_gen != load_id {
                    return;
                }
                match outcome {
                    Ok(Ok(preview)) => this.tabular = TabularPreview::Ready(preview),
                    Ok(Err(e)) => this.tabular = TabularPreview::Error(e.to_string()),
                    Err(e) => this.tabular = TabularPreview::Error(e.to_string()),
                }
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "Failed to apply tabular preview"))
            .ok();
        })
        .detach();
    }

    fn turn_pdf_page(&mut self, next: bool, cx: &mut Context<Self>) {
        let PdfPreview::Ready { page, total, .. } = &self.pdf else {
            return;
        };
        let new_page = if next {
            page.saturating_add(1).min(total.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if new_page == *page {
            return;
        }
        self.start_pdf_load(new_page, cx);
        cx.notify();
    }
}

fn headings_to_tree_items(headings: &[Heading]) -> Vec<TreeItem> {
    outline_tree(headings)
        .into_iter()
        .map(|(id, label, children)| {
            let mut item = TreeItem::new(id, label).expanded(true);
            for (cid, clabel) in children {
                item = item.child(TreeItem::new(cid, clabel));
            }
            item
        })
        .collect()
}

fn scroll_fraction(handle: &ScrollHandle) -> f32 {
    let max = f32::from(handle.max_offset().height);
    if max <= 0.0 {
        0.0
    } else {
        (-f32::from(handle.offset().y) / max).clamp(0.0, 1.0)
    }
}

fn apply_scroll_fraction(handle: &ScrollHandle, frac: f32) {
    let max = f32::from(handle.max_offset().height);
    handle.set_offset(point(px(0.0), px(-frac.clamp(0.0, 1.0) * max)));
}

fn plain_text(md: &str) -> String {
    md.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            trimmed.trim_start_matches('#').trim().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn document_text_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(1.15))
        .heading_font_size(|level, base| match level {
            1 => base * 1.75,
            2 => base * 1.4,
            3 => base * 1.2,
            _ => base,
        })
}

fn tabular_rendered_body(tabular: &TabularPreview, cx: &App) -> AnyElement {
    match tabular {
        TabularPreview::Idle | TabularPreview::Loading => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Loading preview…")
            .into_any_element(),
        TabularPreview::Error(message) => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        TabularPreview::Ready(preview) => {
            render_table_preview_view("artifact-table", preview, cx).into_any_element()
        }
    }
}

fn image_rendered_body(path: &Path, cx: &App) -> AnyElement {
    if !path.exists() {
        return div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Image not found: {}", path.display()))
            .into_any_element();
    }
    div()
        .id("artifact-image")
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .child(
            img(path.to_path_buf())
                .max_w(px(IMAGE_DISPLAY_WIDTH))
                .max_h(px(520.0))
                .object_fit(ObjectFit::Contain)
                .rounded_md(),
        )
        .into_any_element()
}

fn pdf_rendered_body(pdf: &PdfPreview, entity: Entity<ArtifactView>, cx: &App) -> AnyElement {
    match pdf {
        PdfPreview::Idle | PdfPreview::Loading { .. } => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Rendering page…")
            .into_any_element(),
        PdfPreview::Error(message) => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(message.clone())
            .into_any_element(),
        PdfPreview::Ready {
            page, total, image, ..
        } => {
            let page = *page;
            let total = *total;
            let can_prev = page > 0;
            let can_next = page + 1 < total;
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .justify_start()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("artifact-pdf-prev")
                                .ghost()
                                .small()
                                .label("Prev")
                                .disabled(!can_prev)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity.update(cx, |this, cx| this.turn_pdf_page(false, cx));
                                    }
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Page {} of {}", page + 1, total.max(1))),
                        )
                        .child(
                            Button::new("artifact-pdf-next")
                                .ghost()
                                .small()
                                .label("Next")
                                .disabled(!can_next)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity.update(cx, |this, cx| this.turn_pdf_page(true, cx));
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .id("artifact-pdf-pages")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .child(
                            img(image.clone())
                                .w(px(PDF_PAGE_DISPLAY_WIDTH))
                                .object_fit(ObjectFit::Fill)
                                .rounded_md(),
                        ),
                )
                .into_any_element()
        }
    }
}

fn artifact_code_block(id: &'static str, source: &str, path: Option<&PathBuf>) -> AnyElement {
    let language = path.and_then(|p| artifact_language_for_path(p));
    let block = if language.is_some() {
        CodeBlockComponent::new(language, source.to_string(), 0)
    } else {
        CodeBlockComponent::plain(None, source.to_string(), 0)
    };
    div()
        .id(ElementId::Name(id.into()))
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .child(block)
        .into_any_element()
}

fn artifact_primary_body(
    path: Option<&PathBuf>,
    rendered: &str,
    source: &str,
    full: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match path {
        Some(path) if is_code_artifact_path(path) => {
            artifact_code_block("artifact-code", source, Some(path))
        }
        Some(path) if is_markdown_artifact_path(path) => {
            let md = TextView::markdown("artifact-md", rendered.to_string(), window, cx)
                .style(document_text_style())
                .selectable(true);
            let inner = if full {
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(div().w_full().max_w(px(680.)).child(md))
            } else {
                div().w_full().child(md)
            };
            inner.into_any_element()
        }
        _ => artifact_code_block("artifact-plain", source, path),
    }
}

impl Render for ArtifactView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mode == ArtifactMode::Closed {
            return div().into_any_element();
        }
        self.refresh_staleness();
        self.apply_pending_scroll();
        let tab = self.tab;
        let source = self.source.clone();
        let rendered = self.rendered.clone();
        let old = self.old.clone();
        let full = self.mode == ArtifactMode::Full;
        let entity = cx.entity();
        let is_pdf = self.path.as_ref().is_some_and(|path| is_pdf_path(path));
        let is_chart = self.chart.is_some();
        let is_image = !is_chart && self.path.as_ref().is_some_and(|path| is_image_path(path));
        let is_tabular = matches!(self.tabular, TabularPreview::Ready(_))
            || self.path.as_ref().is_some_and(|path| is_tabular_path(path));
        let is_markdown = self
            .path
            .as_ref()
            .is_some_and(|path| is_markdown_artifact_path(path));
        let path_ref = self.path.clone();
        let pdf = self.pdf.clone();
        let tabular = self.tabular.clone();
        let chart = self.chart.clone();
        let has_diff = !old.is_empty() && old != source;
        let visible_tab = if !has_diff { tab.min(1) } else { tab };
        let show_outline = full && is_markdown && !parse_headings(&source).is_empty();
        let stale = self.stale;
        let primary_label = if is_tabular {
            "Table"
        } else {
            path_ref
                .as_ref()
                .map(|path| {
                    if is_code_artifact_path(path) {
                        "Code"
                    } else if is_markdown_artifact_path(path) {
                        "Rendered"
                    } else {
                        "Preview"
                    }
                })
                .unwrap_or("Preview")
        };

        let copy_source = self.source.clone();
        let copy_plain = plain_text(&self.source);

        let document_body = if is_pdf {
            pdf_rendered_body(&pdf, entity.clone(), cx)
        } else if let Some(spec) = chart {
            div()
                .id("artifact-chart")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&self.body_scroll)
                .child(render_chart_panel(spec, cx))
                .into_any_element()
        } else if is_image {
            let image_path = path_ref.clone().unwrap_or_default();
            image_rendered_body(&image_path, cx)
        } else {
            div()
                .id("artifact-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&self.body_scroll)
                .p_2()
                .when(visible_tab == 0, |this| {
                    if is_tabular {
                        this.child(tabular_rendered_body(&tabular, cx))
                    } else {
                        this.child(artifact_primary_body(
                            path_ref.as_ref(),
                            &rendered,
                            &source,
                            full,
                            window,
                            cx,
                        ))
                    }
                })
                .when(visible_tab == 1, |this| {
                    this.child(artifact_code_block(
                        "artifact-source",
                        &source,
                        path_ref.as_ref(),
                    ))
                })
                .when(has_diff && visible_tab == 2, |this| {
                    this.child(DiffHunkList::new(
                        "artifact-diff",
                        old.clone(),
                        source.clone(),
                    ))
                })
                .into_any_element()
        };

        let tabs = if is_pdf || is_chart || is_image {
            div().into_any_element()
        } else {
            TabBar::new("artifact-modes")
                .segmented()
                .child(Tab::new().label(primary_label))
                .child(Tab::new().label("Source"))
                .when(has_diff, |this| this.child(Tab::new().label("Diff")))
                .selected_index(visible_tab)
                .on_click({
                    let entity = entity.clone();
                    move |ix, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.set_tab(*ix, has_diff, cx);
                        });
                    }
                })
                .into_any_element()
        };

        let outline = if show_outline {
            let tree_state = self.tree_state.clone();
            Some(
                div()
                    .w(px(220.))
                    .h_full()
                    .min_h_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(tree(&tree_state, {
                        let entity = entity.clone();
                        move |ix, entry, selected, _window, cx| {
                            let id = entry.item().id.clone();
                            let label = entry.item().label.clone();
                            let entity = entity.clone();
                            ListItem::new(ix)
                                .selected(selected)
                                .pl(px(12.0 + 12.0 * entry.depth() as f32))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if selected {
                                            cx.theme().foreground
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(label),
                                )
                                .on_click(move |_, _, cx| {
                                    if let Ok(line) = id.to_string().parse::<u32>() {
                                        entity.update(cx, |this, cx| {
                                            this.jump_to_line(line, has_diff, cx);
                                        });
                                    }
                                })
                        }
                    })),
            )
        } else {
            None
        };

        let body = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(tabs)
            .when(stale, |this| {
                let entity = entity.clone();
                this.child(
                    div()
                        .px_2()
                        .pt_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            Alert::warning("artifact-stale", "This file has changed on disk.")
                                .banner(),
                        )
                        .child(
                            Button::new("artifact-reload")
                                .small()
                                .label("Reload")
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |this, cx| this.reload_from_disk(cx));
                                }),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when_some(outline, |this, rail| this.child(rail))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(document_body),
                    ),
            )
            .when(full, |this| {
                this.child(RunPin::new(RunPinKind::JumpToLatest).visible(true))
            })
            .into_any_element();

        let title = self
            .chart
            .as_ref()
            .and_then(|spec| spec.title.clone())
            .or_else(|| {
                self.path
                    .as_ref()
                    .map(|p| panel_title(p, self.chart.as_ref().and_then(|c| c.title.as_deref())))
            })
            .or_else(|| {
                if let TabularPreview::Ready(preview) = &self.tabular {
                    Some(preview.title.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                if self.chart.is_some() {
                    "Chart".to_string()
                } else {
                    "Document".to_string()
                }
            });
        let meta = self.path.as_ref().map(|p| card_meta(p));
        let file_buttons: Vec<AnyElement> = self
            .files
            .iter()
            .map(|(path, source, old)| {
                let label = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let entity = entity.clone();
                let path = path.clone();
                let source = source.clone();
                let old = old.clone();
                Button::new(ElementId::Name(format!("artifact-file-{}", label).into()))
                    .ghost()
                    .small()
                    .label(label)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            let workspace = this.workspace_root.clone();
                            this.open(path.clone(), source.clone(), old.clone(), workspace, cx);
                        });
                    })
                    .into_any_element()
            })
            .collect();

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family("monospace")
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when_some(meta, |this, meta| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(meta),
                        )
                    })
                    .child(
                        DropdownButton::new("artifact-copy")
                            .ghost()
                            .small()
                            .button(
                                Button::new("artifact-copy-main")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Copy).size_3())
                                    .label("Copy")
                                    .on_click({
                                        let copy = copy_source.clone();
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy.clone(),
                                            ));
                                        }
                                    }),
                            )
                            .dropdown_menu({
                                let markdown = copy_source.clone();
                                let rendered_copy = copy_source.clone();
                                let plain = copy_plain.clone();
                                move |menu, _, _| {
                                    menu.item(PopupMenuItem::new("Markdown").on_click({
                                        let markdown = markdown.clone();
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                markdown.clone(),
                                            ));
                                        }
                                    }))
                                    .item(PopupMenuItem::new("Rendered").on_click({
                                        let rendered_copy = rendered_copy.clone();
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                rendered_copy.clone(),
                                            ));
                                        }
                                    }))
                                    .item(
                                        PopupMenuItem::new("Plain").on_click({
                                            let plain = plain.clone();
                                            move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    plain.clone(),
                                                ));
                                            }
                                        }),
                                    )
                                }
                            }),
                    )
                    .child(
                        Button::new("artifact-expand")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(if full {
                                    IconName::Minimize
                                } else {
                                    IconName::Maximize
                                })
                                .size_3(),
                            )
                            .tooltip(if full {
                                "Exit full window"
                            } else {
                                "Full window"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                let next = if this.mode == ArtifactMode::Full {
                                    ArtifactMode::Docked
                                } else {
                                    ArtifactMode::Full
                                };
                                this.set_mode(next, cx);
                            })),
                    )
                    .child(
                        Button::new("artifact-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Close).size_3())
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_mode(ArtifactMode::Closed, cx);
                            })),
                    ),
            )
            .when(file_buttons.len() > 1, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_1()
                        .children(file_buttons),
                )
            });

        let panel = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(280.))
            .w_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(header)
            .child(body);

        if full {
            div().size_full().child(panel).into_any_element()
        } else {
            panel.into_any_element()
        }
    }
}

impl EventEmitter<ArtifactViewEvent> for ArtifactView {}

pub fn new_artifact_view(cx: &mut App) -> Entity<ArtifactView> {
    cx.new(ArtifactView::new)
}
