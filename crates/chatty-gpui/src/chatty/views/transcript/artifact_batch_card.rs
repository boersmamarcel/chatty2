use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::OpenArtifact;
use super::artifact_kind::{artifact_file_name, artifact_meta_line, read_artifact_source};

#[derive(IntoElement)]
pub struct ArtifactBatchCard {
    files: Vec<(PathBuf, Option<String>)>,
    open_path: Option<PathBuf>,
    on_open: Option<OpenArtifact>,
}

impl ArtifactBatchCard {
    pub fn new(files: Vec<(PathBuf, Option<String>)>) -> Self {
        Self {
            files,
            open_path: None,
            on_open: None,
        }
    }

    pub fn open_path(mut self, path: Option<PathBuf>) -> Self {
        self.open_path = path;
        self
    }

    pub fn on_open(mut self, f: impl Fn(super::ArtifactOpen, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for ArtifactBatchCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.files.is_empty() {
            return div().into_any_element();
        }
        if self.files.len() == 1 {
            let (path, old) = self.files[0].clone();
            let open = self.open_path.as_ref().is_some_and(|open| open == &path);
            let mut card = super::artifact_card::ArtifactCard::new(path)
                .old_content(old)
                .open(open);
            if let Some(on_open) = self.on_open {
                card = card.on_open(move |payload, cx| on_open(payload, cx));
            }
            return card.into_any_element();
        }

        let count = self.files.len();
        let noun = if count == 1 { "file" } else { "files" };
        let on_open = self.on_open;
        let open_path = self.open_path;
        let batch_open = self
            .files
            .iter()
            .any(|(path, _)| open_path.as_ref().is_some_and(|open| open == path));

        div()
            .id(ElementId::Name(format!("artifact-batch-{count}").into()))
            .w_full()
            .rounded(px(16.))
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .flex()
            .flex_row()
            .child(div().w(px(4.)).bg(if batch_open {
                cx.theme().accent
            } else {
                cx.theme().transparent
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_2()
                            .child(
                                Icon::new(IconName::File)
                                    .size_4()
                                    .text_color(cx.theme().muted_foreground),
                            )
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
                                            .child(format!("{count} {noun} produced")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Click a file to open in the panel"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .px_2()
                            .pb_2()
                            .max_h(px(140.))
                            .overflow_y_scrollbar()
                            .children(self.files.into_iter().enumerate().map(
                                |(ix, (path, old))| {
                                    let label = artifact_file_name(&path);
                                    let meta = artifact_meta_line(&path);
                                    let on_open_row = on_open.clone();
                                    let on_open_btn = on_open.clone();
                                    let path_for_open = path.clone();
                                    let old_for_open = old.clone();
                                    let row_open = open_path
                                        .as_ref()
                                        .is_some_and(|open| open == &path_for_open);
                                    div()
                                        .id(ElementId::Name(
                                            format!("artifact-batch-row-{ix}").into(),
                                        ))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .when(row_open, |this| this.bg(cx.theme().muted))
                                        .on_mouse_down(MouseButton::Left, {
                                            let path = path_for_open.clone();
                                            let old = old_for_open.clone();
                                            let on_open_row = on_open_row.clone();
                                            move |_, _, cx| {
                                                if let Some(cb) = on_open_row.as_ref() {
                                                    cb(
                                                        super::ArtifactOpen {
                                                            path: path.clone(),
                                                            source: read_artifact_source(&path),
                                                            old: old.clone(),
                                                        },
                                                        cx,
                                                    );
                                                }
                                            }
                                        })
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
                                                        .child(label),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(meta),
                                                ),
                                        )
                                        .child(
                                            Button::new(ElementId::Name(
                                                format!("artifact-batch-open-{ix}").into(),
                                            ))
                                            .ghost()
                                            .small()
                                            .label("Open")
                                            .on_click({
                                                let path = path_for_open;
                                                let old = old_for_open;
                                                let on_open_btn = on_open_btn.clone();
                                                move |_, _, cx| {
                                                    if let Some(cb) = on_open_btn.as_ref() {
                                                        cb(
                                                            super::ArtifactOpen {
                                                                path: path.clone(),
                                                                source: read_artifact_source(&path),
                                                                old: old.clone(),
                                                            },
                                                            cx,
                                                        );
                                                    }
                                                }
                                            }),
                                        )
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}
