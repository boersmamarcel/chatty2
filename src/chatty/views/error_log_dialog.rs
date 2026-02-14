use crate::chatty::models::error_store::{ErrorEntry, ErrorLevel, ErrorStore};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Root, Sizable, button::Button, h_flex, scroll::ScrollableElement, v_flex,
};
use std::time::{SystemTime, UNIX_EPOCH};

fn format_timestamp(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let secs = duration.as_secs();
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            let seconds = secs % 60;
            format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
        }
        Err(_) => "Unknown time".to_string(),
    }
}

pub struct ErrorLogDialog;

impl ErrorLogDialog {
    pub fn new() -> Self {
        Self
    }

    pub fn open(cx: &mut App) {
        let dialog = cx.new(|_cx| Self::new());

        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Errors & Warnings".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    size: size(px(700.0), px(500.0)),
                    origin: point(px(0.0), px(0.0)),
                })),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Root::new(dialog, window, cx)),
        )
        .map_err(|e| tracing::warn!(error = ?e, "Failed to open error log dialog"))
        .ok();
    }
}

impl Render for ErrorLogDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = cx.global::<ErrorStore>().get_all_entries();
        let entries_reversed: Vec<_> = entries.into_iter().rev().collect(); // Most recent first

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(
                // Header with "Clear All" button
                div()
                    .h(px(48.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(16.0))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_lg()
                            .text_color(cx.theme().foreground)
                            .child("Errors & Warnings"),
                    )
                    .child(
                        Button::new("clear-all")
                            .small()
                            .on_click(|_event, _window, cx| {
                                cx.update_global::<ErrorStore, _>(|store, _cx| {
                                    store.clear();
                                });
                                cx.refresh_windows();
                            })
                            .child("Clear All"),
                    ),
            )
            .child(
                // Scrollable error list
                div()
                    .id("error-list")
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px(px(16.0))
                    .py(px(12.0))
                    .when(entries_reversed.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No errors or warnings to display."),
                        )
                    })
                    .when(!entries_reversed.is_empty(), |this| {
                        this.children(
                            entries_reversed
                                .into_iter()
                                .enumerate()
                                .map(|(ix, entry)| ErrorEntryView::new(entry).id(ix)),
                        )
                    }),
            )
    }
}

#[derive(IntoElement)]
struct ErrorEntryView {
    entry: ErrorEntry,
    id: usize,
}

impl ErrorEntryView {
    fn new(entry: ErrorEntry) -> Self {
        Self { entry, id: 0 }
    }

    fn id(mut self, id: usize) -> Self {
        self.id = id;
        self
    }
}

impl RenderOnce for ErrorEntryView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let level_color = if self.entry.level == ErrorLevel::Error {
            cx.theme().accent
        } else {
            cx.theme().muted_foreground
        };

        let level_text = if self.entry.level == ErrorLevel::Error {
            "ERROR"
        } else {
            "WARN"
        };

        div()
            .id(self.id)
            .mb_3()
            .p_3()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(
                // Header row: timestamp, level badge, target
                h_flex()
                    .gap_2()
                    .items_center()
                    .mb_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format_timestamp(self.entry.timestamp)),
                    )
                    .child(
                        div().px_2().py_1().rounded_sm().bg(level_color).child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(cx.theme().background)
                                .child(level_text),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.entry.target.clone()),
                    ),
            )
            .child(
                // Message
                div()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .mb_2()
                    .child(self.entry.message.clone()),
            )
            .when(
                self.entry.file.is_some() || self.entry.line.is_some(),
                |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .mb_2()
                            .child(format!(
                                "{}:{}",
                                self.entry.file.clone().unwrap_or_default(),
                                self.entry.line.unwrap_or_default()
                            )),
                    )
                },
            )
            .when(!self.entry.fields.is_empty(), |this| {
                this.child(v_flex().gap_1().children(self.entry.fields.iter().map(
                    |(key, value)| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{}: {}", key, value))
                    },
                )))
            })
    }
}
