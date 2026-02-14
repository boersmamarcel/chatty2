use crate::chatty::models::error_store::{ErrorEntry, ErrorLevel, ErrorStore};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    scroll::ScrollableElement,
    v_flex,
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
    pub fn open(window: &mut Window, cx: &mut App) {
        window.open_dialog(cx, |dialog, _window, cx| {
            let entries = cx.global::<ErrorStore>().get_all_entries();
            let entries_reversed: Vec<_> = entries.into_iter().rev().collect(); // Most recent first

            dialog
                .title("Errors & Warnings")
                .w(px(700.0))
                .h(px(500.0))
                .child(
                    div()
                        .id("error-list")
                        .h_full()
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
                .footer(|_, window, _, _cx| {
                    vec![Button::new("clear-all").label("Clear All").on_click({
                        move |_, window, cx| {
                            cx.update_global::<ErrorStore, _>(|store, _cx| {
                                store.clear();
                            });
                            cx.refresh_windows();
                            window.close_dialog(cx);
                        }
                    })]
                })
        });
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
            cx.theme().ring
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
