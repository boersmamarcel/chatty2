use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::OpenArtifact;
use super::artifact_kind::{is_image_path, is_tabular_path, read_artifact_source};
use super::artifact_meta::{
    card_meta, display_title, file_name, format_tabular_shape, peek_lines, reveal_in_file_manager,
    tabular_shape,
};

const IMAGE_THUMB_HEIGHT: f32 = 120.0;

#[cfg(target_os = "macos")]
const REVEAL_LABEL: &str = "Reveal in Finder";
#[cfg(target_os = "windows")]
const REVEAL_LABEL: &str = "Reveal in Explorer";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const REVEAL_LABEL: &str = "Reveal";

#[derive(IntoElement)]
pub struct ArtifactCard {
    path: PathBuf,
    display_title: Option<String>,
    old_content: Option<String>,
    open: bool,
    on_open: Option<OpenArtifact>,
}

impl ArtifactCard {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            display_title: None,
            old_content: None,
            open: false,
            on_open: None,
        }
    }

    pub fn display_title(mut self, title: impl Into<String>) -> Self {
        self.display_title = Some(title.into());
        self
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

impl RenderOnce for ArtifactCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let name = file_name(&self.path);
        let title = display_title(&self.path, self.display_title.as_deref());
        let meta = card_meta(&self.path);
        let source = read_artifact_source(&self.path);
        let peek = peek_lines(&source, 20);
        let extra = if is_tabular_path(&self.path) {
            tabular_shape(&source, &self.path).map(|(rows, cols)| format_tabular_shape(rows, cols))
        } else {
            None
        };
        let is_image = is_image_path(&self.path);
        let show_file_name = name != title;
        let path = self.path.clone();
        let on_open = self.on_open.clone();
        let old_content = self.old_content.clone();
        let open = self.open;
        let open_id = format!("artifact-card-{}", path.display());
        let fire_open = {
            let path = path.clone();
            let on_open = on_open.clone();
            let old_content = old_content.clone();
            move |cx: &mut App| {
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
        };

        let accent = cx.theme().info;
        div()
            .id(ElementId::Name(open_id.into()))
            .relative()
            .w_full()
            .overflow_hidden()
            .rounded(px(16.))
            .border_1()
            .border_color(cx.theme().border)
            .px_3()
            .py_2()
            .flex()
            .flex_row()
            .items_start()
            .gap_2()
            .when(open, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(4.))
                        .bg(accent),
                )
            })
            .child(
                div().absolute().left_2().bottom(px(-10.)).child(
                    Icon::new(IconName::GalleryVerticalEnd)
                        .size_6()
                        .text_color(cx.theme().muted_foreground),
                ),
            )
            .child(div().w(px(28.)).flex_shrink_0())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, {
                        let fire_open = fire_open.clone();
                        move |_, _, cx| fire_open(cx)
                    })
                    .child({
                        let mut title_btn = Button::new(ElementId::Name(
                            format!("artifact-title-{}", path.display()).into(),
                        ))
                        .ghost()
                        .compact()
                        .label(title);
                        if !peek.is_empty() && !is_image {
                            title_btn = title_btn.tooltip(peek);
                        }
                        title_btn
                    })
                    .when(show_file_name, |this| {
                        this.child(
                            div()
                                .font_family("monospace")
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(name),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta),
                    )
                    .when_some(extra, |this, line| {
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
                                .h(px(IMAGE_THUMB_HEIGHT))
                                .max_w(px(220.))
                                .object_fit(ObjectFit::Contain)
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
                    .child(
                        Button::new(ElementId::Name(
                            format!("artifact-reveal-{}", path.display()).into(),
                        ))
                        .ghost()
                        .small()
                        .label("Reveal")
                        .tooltip(REVEAL_LABEL)
                        .on_click({
                            let path = path.clone();
                            move |_, _, cx| {
                                cx.stop_propagation();
                                reveal_in_file_manager(&path);
                            }
                        }),
                    )
                    .child(
                        Clipboard::new(ElementId::Name(
                            format!("artifact-copy-path-{}", path.display()).into(),
                        ))
                        .value(path.display().to_string()),
                    )
                    .child(
                        Button::new(ElementId::Name(
                            format!("artifact-open-{}", path.display()).into(),
                        ))
                        .ghost()
                        .small()
                        .label("Open in split")
                        .on_click({
                            let fire_open = fire_open.clone();
                            move |_, _, cx| {
                                cx.stop_propagation();
                                fire_open(cx);
                            }
                        }),
                    ),
            )
    }
}
