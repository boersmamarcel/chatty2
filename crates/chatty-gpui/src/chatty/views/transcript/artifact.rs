use std::path::PathBuf;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

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
}

impl ArtifactCard {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl RenderOnce for ArtifactCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let name = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        let path_display = self.path.display().to_string();
        Popover::new(ElementId::Name(
            format!("artifact-peek-{path_display}").into(),
        ))
        .trigger(
            Button::new(ElementId::Name(
                format!("artifact-card-{path_display}").into(),
            ))
            .ghost()
            .small()
            .icon(Icon::new(IconName::File).size_3())
            .label(name),
        )
        .appearance(false)
        .content(move |_, _, cx| {
            div()
                .p_2()
                .max_w(px(360.))
                .bg(cx.theme().popover)
                .text_xs()
                .child(path_display.clone())
        })
    }
}

/// One artifact workbench entity: Closed | Docked | Full. Reparent, do not rebuild.
pub struct ArtifactView {
    pub mode: ArtifactMode,
    pub path: Option<PathBuf>,
    pub rendered: String,
    pub source: String,
    pub old: String,
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
            tab: 0,
        }
    }

    pub fn open(&mut self, path: PathBuf, source: String, cx: &mut Context<Self>) {
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
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
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
            );

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
