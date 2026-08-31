use std::path::{Path, PathBuf};
use std::rc::Rc;

use chatty_core::services::pdf_thumbnail::{
    PREVIEW_WIDTH, PdfThumbnailError, pdf_page_count, render_pdf_page,
};
use chatty_core::tools::chart_tool::ChartSpec;
use chatty_core::tools::data_query_tool::{
    FILE_PREVIEW_MAX_ROWS, TablePreview, load_file_table_preview,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use tracing::warn;

use super::OpenArtifact;
use super::artifact_kind::{
    artifact_language_for_path, is_code_artifact_path, is_image_path, is_markdown_artifact_path,
    is_pdf_path, is_tabular_path, read_artifact_source,
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

#[derive(IntoElement)]
pub struct ArtifactCard {
    path: PathBuf,
    old_content: Option<String>,
    on_open: Option<OpenArtifact>,
}

impl ArtifactCard {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            old_content: None,
            on_open: None,
        }
    }

    pub fn old_content(mut self, old: Option<String>) -> Self {
        self.old_content = old;
        self
    }

    pub fn on_open(mut self, f: impl Fn(super::ArtifactOpen, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for ArtifactCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let name = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        let kind = self
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_uppercase())
            .unwrap_or_else(|| "FILE".to_string());
        let path = self.path.clone();
        let on_open = self.on_open.clone();
        let old_content = self.old_content.clone();
        div()
            .id(ElementId::Name(
                format!("artifact-card-{}", self.path.display()).into(),
            ))
            .w_full()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().border)
            .px_3()
            .py_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, {
                let path = path.clone();
                let on_open = on_open.clone();
                let old_content = old_content.clone();
                move |_, _, cx| {
                    if let Some(cb) = on_open.as_ref() {
                        let source = read_artifact_source(&path);
                        cb(
                            super::ArtifactOpen {
                                path: path.clone(),
                                source,
                                old: old_content.clone(),
                            },
                            cx,
                        );
                    }
                }
            })
            .child(Icon::new(IconName::File).size_4())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Document · {kind} · open in panel")),
                    ),
            )
            .child(
                Button::new(ElementId::Name(
                    format!("artifact-open-{}", self.path.display()).into(),
                ))
                .ghost()
                .small()
                .label("Open")
                .on_click({
                    let path = path.clone();
                    let on_open = on_open.clone();
                    let old_content = old_content.clone();
                    move |_, _, cx| {
                        if let Some(cb) = on_open.as_ref() {
                            let source = read_artifact_source(&path);
                            cb(
                                super::ArtifactOpen {
                                    path: path.clone(),
                                    source,
                                    old: old_content.clone(),
                                },
                                cx,
                            );
                        }
                    }
                }),
            )
    }
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
}

impl ArtifactView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
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
        }
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
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
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
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
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
        let old_snapshot = old.clone();
        if !self.files.iter().any(|(existing, _, _)| existing == &path) {
            self.files
                .push((path.clone(), source.clone(), old_snapshot.clone()));
        }
        self.path = Some(path.clone());
        self.workspace_root = workspace_root.clone();
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
            self.start_tabular_load(path, workspace_root, cx);
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
            self.tab = if old_snapshot
                .as_ref()
                .is_some_and(|o| !o.is_empty() && o != &source)
            {
                2
            } else {
                0
            };
        }
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
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
        self.mode = mode;
        cx.notify();
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
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match path {
        Some(path) if is_code_artifact_path(path) => {
            artifact_code_block("artifact-code", source, Some(path))
        }
        Some(path) if is_markdown_artifact_path(path) => {
            TextView::markdown("artifact-md", rendered.to_string(), window, cx).into_any_element()
        }
        _ => artifact_code_block("artifact-plain", source, path),
    }
}

impl Render for ArtifactView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mode == ArtifactMode::Closed {
            return div().into_any_element();
        }
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
        let path_ref = self.path.clone();
        let pdf = self.pdf.clone();
        let tabular = self.tabular.clone();
        let chart = self.chart.clone();
        let has_diff = !old.is_empty() && old != source;
        let visible_tab = if !has_diff && tab > 1 {
            0
        } else if !has_diff && tab > 0 {
            tab.min(1)
        } else {
            tab
        };
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

        let body = if is_pdf {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .child(pdf_rendered_body(&pdf, entity.clone(), cx))
                .when(full, |this| {
                    this.child(RunPin::new(RunPinKind::JumpToLatest).visible(true))
                })
                .into_any_element()
        } else if let Some(spec) = chart {
            div()
                .id("artifact-chart")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .overflow_y_scroll()
                .child(render_chart_panel(spec, cx))
                .when(full, |this| {
                    this.child(RunPin::new(RunPinKind::JumpToLatest).visible(true))
                })
                .into_any_element()
        } else if is_image {
            let image_path = path_ref.clone().unwrap_or_default();
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_2()
                .child(image_rendered_body(&image_path, cx))
                .when(full, |this| {
                    this.child(RunPin::new(RunPinKind::JumpToLatest).visible(true))
                })
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
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
                                    this.tab = if has_diff { *ix } else { (*ix).min(1) };
                                    cx.notify();
                                });
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .p_2()
                        .when(visible_tab == 0, |this| {
                            if is_tabular {
                                this.child(tabular_rendered_body(&tabular, cx))
                            } else {
                                this.child(artifact_primary_body(
                                    path_ref.as_ref(),
                                    &rendered,
                                    &source,
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
                            this.child(DiffHunkList::new("artifact-diff", old, source))
                        }),
                )
                .child(
                    Button::new("artifact-copy")
                        .ghost()
                        .small()
                        .label("Copy")
                        .on_click({
                            let copy = self.source.clone();
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }
                        }),
                )
                .when(full, |this| {
                    this.child(RunPin::new(RunPinKind::JumpToLatest).visible(true))
                })
                .into_any_element()
        };

        let title = self
            .chart
            .as_ref()
            .and_then(|spec| spec.title.clone())
            .or_else(|| {
                self.path
                    .as_ref()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
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
                    .when(is_pdf, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("PDF"),
                        )
                    })
                    .when(is_chart, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Chart"),
                        )
                    })
                    .when(is_image, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Image"),
                        )
                    })
                    .child(
                        Button::new("artifact-close")
                            .ghost()
                            .small()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mode = ArtifactMode::Closed;
                                cx.emit(ArtifactViewEvent::Closed);
                                cx.notify();
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
