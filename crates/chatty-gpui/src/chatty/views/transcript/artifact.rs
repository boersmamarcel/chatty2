use std::path::PathBuf;
use std::rc::Rc;

use chatty_core::services::pdf_thumbnail::{
    PREVIEW_WIDTH, PdfThumbnailError, pdf_page_count, render_pdf_page,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use tracing::warn;

use super::OpenArtifact;
use super::artifact_kind::{is_pdf_path, read_artifact_source};
use super::diff::DiffHunkList;
use super::run_pin::{RunPin, RunPinKind};

/// Inner width of the 380px dock minus body padding. `img` only derives height
/// from aspect ratio when width is an absolute `px()` — `w_full()` is relative,
/// so GPUI would keep the PNG's pixel height and `ObjectFit::Contain` would
/// center the sheet in that tall box (page sitting in the lower half).
const PDF_PAGE_DISPLAY_WIDTH: f32 = 348.0;

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

#[derive(IntoElement)]
pub struct ArtifactCard {
    path: PathBuf,
    on_open: Option<OpenArtifact>,
}

impl ArtifactCard {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            on_open: None,
        }
    }

    pub fn on_open(mut self, f: impl Fn(PathBuf, String, &mut App) + 'static) -> Self {
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
                move |_, _, cx| {
                    if let Some(cb) = on_open.as_ref() {
                        let source = read_artifact_source(&path);
                        cb(path.clone(), source, cx);
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
                .on_click(move |_, _, cx| {
                    if let Some(cb) = on_open.as_ref() {
                        let source = read_artifact_source(&path);
                        cb(path.clone(), source, cx);
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
    files: Vec<(PathBuf, String)>,
    tab: usize,
    pdf: PdfPreview,
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
            load_gen: 0,
        }
    }

    pub fn open(&mut self, path: PathBuf, source: String, cx: &mut Context<Self>) {
        if !self.files.iter().any(|(existing, _)| existing == &path) {
            self.files.push((path.clone(), source.clone()));
        }
        self.path = Some(path.clone());
        if is_pdf_path(&path) {
            self.source.clear();
            self.rendered.clear();
            self.old.clear();
            self.tab = 0;
            self.start_pdf_load(0, cx);
        } else {
            self.pdf = PdfPreview::Idle;
            self.source = source.clone();
            self.rendered = source;
        }
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
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
        let pdf = self.pdf.clone();

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
                        .child(Tab::new().label("Rendered"))
                        .child(Tab::new().label("Source"))
                        .child(Tab::new().label("Diff"))
                        .selected_index(tab)
                        .on_click({
                            let entity = entity.clone();
                            move |ix, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.tab = *ix;
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
                        .when(tab == 0, |this| {
                            this.child(TextView::markdown("artifact-md", rendered, window, cx))
                        })
                        .when(tab == 1, |this| {
                            this.child(
                                div()
                                    .id("artifact-source")
                                    .font_family("monospace")
                                    .text_xs()
                                    .overflow_y_scroll()
                                    .child(source.clone()),
                            )
                        })
                        .when(tab == 2, |this| {
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
        };

        let title = self
            .path
            .as_ref()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "Document".to_string());
        let file_buttons: Vec<AnyElement> = self
            .files
            .iter()
            .map(|(path, source)| {
                let label = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let entity = entity.clone();
                let path = path.clone();
                let source = source.clone();
                Button::new(ElementId::Name(format!("artifact-file-{}", label).into()))
                    .ghost()
                    .small()
                    .label(label)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.open(path.clone(), source.clone(), cx);
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
            .w(px(380.))
            .min_w(px(280.))
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
