use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::OpenArtifact;
use super::artifact_kind::{
    artifact_file_name, artifact_meta_line, artifact_peek_lines, csv_stat_line, is_image_path,
    is_pdf_path, is_tabular_path, read_artifact_source,
};

const IMAGE_THUMB_PX: f32 = 120.0;
const PEEK_LINES: usize = 20;

#[derive(IntoElement)]
pub struct ArtifactCard {
    path: PathBuf,
    old_content: Option<String>,
    open: bool,
    on_open: Option<OpenArtifact>,
}

impl ArtifactCard {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            old_content: None,
            open: false,
            on_open: None,
        }
    }

    pub fn old_content(mut self, old: Option<String>) -> Self {
        self.old_content = old;
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn on_open(mut self, f: impl Fn(super::ArtifactOpen, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(f));
        self
    }
}

fn open_payload(
    path: &Path,
    old_content: &Option<String>,
    on_open: &Option<OpenArtifact>,
    cx: &mut App,
) {
    if let Some(cb) = on_open.as_ref() {
        cb(
            super::ArtifactOpen {
                path: path.to_path_buf(),
                source: read_artifact_source(path),
                old: old_content.clone(),
            },
            cx,
        );
    }
}

pub fn reveal_path_in_os(path: &Path) {
    let result = {
        #[cfg(target_os = "macos")]
        {
            Command::new("open").arg("-R").arg(path).spawn()
        }
        #[cfg(target_os = "windows")]
        {
            Command::new("explorer").arg("/select,").arg(path).spawn()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let parent = path.parent().unwrap_or(path);
            Command::new("xdg-open").arg(parent).spawn()
        }
    };
    if let Err(error) = result {
        tracing::warn!(error = ?error, path = %path.display(), "Failed to reveal artifact path");
    }
}

impl RenderOnce for ArtifactCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let path = self.path.clone();
        let file_name = artifact_file_name(&path);
        let meta = artifact_meta_line(&path);
        let on_open = self.on_open.clone();
        let old_content = self.old_content.clone();
        let open = self.open;
        let is_image = is_image_path(&path);
        let csv_line = if is_tabular_path(&path) {
            csv_stat_line(&read_artifact_source(&path))
        } else {
            None
        };
        let peek = if !is_image && !is_pdf_path(&path) {
            let text = read_artifact_source(&path);
            let peek = artifact_peek_lines(&text, PEEK_LINES);
            (!peek.is_empty()).then_some(peek)
        } else {
            None
        };
        let path_display = path.display().to_string();
        let card_id = format!("artifact-card-{}", path_display);

        let glyph = Popover::new(ElementId::Name(
            format!("artifact-peek-{path_display}").into(),
        ))
        .trigger(
            Button::new(ElementId::Name(
                format!("artifact-glyph-{path_display}").into(),
            ))
            .ghost()
            .icon(Icon::new(IconName::File).size_6())
            .tooltip("Peek"),
        )
        .content({
            let peek = peek.clone().unwrap_or_else(|| file_name.clone());
            move |_, _, cx| {
                div()
                    .p_3()
                    .max_w(px(420.))
                    .max_h(px(240.))
                    .overflow_hidden()
                    .bg(cx.theme().popover)
                    .font_family("monospace")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(peek.clone())
            }
        });

        div()
            .id(ElementId::Name(card_id.into()))
            .w_full()
            .rounded(px(16.))
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .flex()
            .flex_row()
            .child(div().w(px(4.)).bg(if open {
                cx.theme().accent
            } else {
                cx.theme().transparent
            }))
            .child(
                div()
                    .w(px(36.))
                    .overflow_hidden()
                    .flex()
                    .items_end()
                    .justify_center()
                    .mb(px(-10.))
                    .child(glyph),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_2()
                    .gap_1()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, {
                        let path = path.clone();
                        let on_open = on_open.clone();
                        let old_content = old_content.clone();
                        move |_, _, cx| {
                            open_payload(&path, &old_content, &on_open, cx);
                        }
                    })
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(file_name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta),
                    )
                    .when_some(csv_line, |this, line| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(line),
                        )
                    })
                    .when(is_image && path.exists(), |this| {
                        this.child(
                            img(path.clone())
                                .w(px(IMAGE_THUMB_PX))
                                .h(px(IMAGE_THUMB_PX))
                                .object_fit(ObjectFit::Cover)
                                .rounded_md(),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new(ElementId::Name(
                            format!("artifact-open-{path_display}").into(),
                        ))
                        .ghost()
                        .small()
                        .label("Open in split")
                        .on_click({
                            let path = path.clone();
                            let on_open = on_open.clone();
                            let old_content = old_content.clone();
                            move |_, _, cx| {
                                open_payload(&path, &old_content, &on_open, cx);
                            }
                        }),
                    )
                    .child(
                        Button::new(ElementId::Name(
                            format!("artifact-reveal-{path_display}").into(),
                        ))
                        .ghost()
                        .small()
                        .label("Reveal")
                        .on_click({
                            let path = path.clone();
                            move |_, _, _cx| {
                                reveal_path_in_os(&path);
                            }
                        }),
                    )
                    .child(
                        Clipboard::new(ElementId::Name(
                            format!("artifact-copy-path-{path_display}").into(),
                        ))
                        .value(path_display.clone()),
                    ),
            )
    }
}
