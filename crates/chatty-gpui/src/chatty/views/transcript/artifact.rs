use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::OpenArtifact;
use super::diff::DiffHunkList;
use super::run_pin::{RunPin, RunPinKind};

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
                        let source = std::fs::read_to_string(&path).unwrap_or_default();
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
                        let source = std::fs::read_to_string(&path).unwrap_or_default();
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
        }
    }

    pub fn open(&mut self, path: PathBuf, source: String, cx: &mut Context<Self>) {
        if !self.files.iter().any(|(existing, _)| existing == &path) {
            self.files.push((path.clone(), source.clone()));
        }
        self.path = Some(path);
        self.source = source.clone();
        self.rendered = source;
        if self.mode == ArtifactMode::Closed {
            self.mode = ArtifactMode::Docked;
        }
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: ArtifactMode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
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

        let body = div()
            .flex()
            .flex_col()
            .size_full()
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
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .when(tab == 0, |this| {
                        this.child(TextView::markdown("artifact-md", rendered, window, cx))
                    })
                    .when(tab == 1, |this| {
                        this.child(
                            div()
                                .font_family("monospace")
                                .text_xs()
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
            });

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
