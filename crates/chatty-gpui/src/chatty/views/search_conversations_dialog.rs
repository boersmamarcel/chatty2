use chatty_core::models::ConversationsStore;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, WindowExt, h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    v_flex,
};

use crate::chatty::controllers::GlobalChattyApp;
use crate::chatty::views::sidebar_view::SidebarEvent;

/// Stateful view inside the search-conversations dialog. Holds the input,
/// a lowercase query string, and a snapshot of all conversation metadata
/// taken when the dialog was opened (sorted most-recent-first).
pub struct SearchConversationsView {
    input: Entity<InputState>,
    query: String,
    all: Vec<(String, String, Option<f64>)>,
    _sub: Subscription,
}

impl SearchConversationsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search conversations..."));

        let store = cx.global::<ConversationsStore>();
        let count = store.count();
        let all = store.list_recent_metadata(count);

        let _sub = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        });

        Self {
            input,
            query: String::new(),
            all,
            _sub,
        }
    }
}

impl Render for SearchConversationsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query.clone();
        let filtered: Vec<(String, String, Option<f64>)> = self
            .all
            .iter()
            .filter(|(_, title, _)| query.is_empty() || title.to_lowercase().contains(&query))
            .cloned()
            .collect();

        let is_empty = filtered.is_empty();
        let empty_label = if query.is_empty() {
            "No conversations."
        } else {
            "No matches."
        };

        v_flex()
            .size_full()
            .gap_2()
            .child(Input::new(&self.input))
            .child(
                v_flex()
                    .id("search-conversations-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .when(is_empty, |this| {
                        this.child(
                            div()
                                .px_3()
                                .py_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(empty_label),
                        )
                    })
                    .children(
                        filtered
                            .into_iter()
                            .enumerate()
                            .map(|(ix, (id, title, cost))| {
                                let id_for_click = id.clone();
                                div()
                                    .id(ix)
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(cx.theme().secondary))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .text_sm()
                                                    .text_color(cx.theme().foreground)
                                                    .child(title),
                                            )
                                            .when_some(cost, |this, c| {
                                                this.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!("${c:.2}")),
                                                )
                                            }),
                                    )
                                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                        if let Some(app) = cx
                                            .try_global::<GlobalChattyApp>()
                                            .and_then(|g| g.try_upgrade())
                                        {
                                            let id = id_for_click.clone();
                                            app.update(cx, |app, cx| {
                                                app.sidebar_view.update(cx, |_, cx| {
                                                    cx.emit(SidebarEvent::SelectConversation(id));
                                                });
                                            });
                                        }
                                        window.close_dialog(cx);
                                    })
                            }),
                    ),
            )
    }
}

/// Static helper that opens the search-conversations dialog.
///
/// Mirrors the pattern used by [`super::ErrorLogDialog::open`]: creates a
/// stateful view entity and embeds it as the dialog's child so that typing
/// filters the list live without rebuilding the dialog.
pub struct SearchConversationsDialog;

impl SearchConversationsDialog {
    pub fn open(window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| SearchConversationsView::new(window, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("Search Conversations")
                .w(px(560.))
                .h(px(500.))
                .child(view.clone())
        });
    }
}
