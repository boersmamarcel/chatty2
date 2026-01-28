use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme, Collapsible};
use std::sync::Arc;

/// Callback type for conversation actions
pub type ConversationActionCallback = Arc<dyn Fn(&str, &mut App) + Send + Sync>;

/// A single conversation item in the sidebar
#[derive(IntoElement, Clone)]
pub struct ConversationItem {
    id: String,
    title: String,
    is_active: bool,
    on_click: Option<ConversationActionCallback>,
    on_delete: Option<ConversationActionCallback>,
    is_collapsed: bool,
    cost_usd: Option<f64>,
}

impl ConversationItem {
    pub fn new(id: String, title: String) -> Self {
        Self {
            id,
            title,
            is_active: false,
            on_click: None,
            on_delete: None,
            is_collapsed: false,
            cost_usd: None,
        }
    }

    pub fn cost(mut self, cost_usd: Option<f64>) -> Self {
        self.cost_usd = cost_usd;
        self
    }

    pub fn active(mut self, is_active: bool) -> Self {
        self.is_active = is_active;
        self
    }

    pub fn on_click<F>(mut self, callback: F) -> Self
    where
        F: Fn(&str, &mut App) + Send + Sync + 'static,
    {
        self.on_click = Some(Arc::new(callback));
        self
    }

    pub fn on_delete<F>(mut self, callback: F) -> Self
    where
        F: Fn(&str, &mut App) + Send + Sync + 'static,
    {
        self.on_delete = Some(Arc::new(callback));
        self
    }
}

impl Collapsible for ConversationItem {
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.is_collapsed = collapsed;
        self
    }

    fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }
}

impl RenderOnce for ConversationItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id_for_click = self.id.clone();
        let id_for_delete = self.id.clone();
        let on_click = self.on_click.clone();
        let on_delete = self.on_delete.clone();

        let bg_color = if self.is_active {
            cx.theme().secondary
        } else {
            cx.theme().background
        };

        div()
            .id(ElementId::Name(self.id.clone().into()))
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(bg_color)
            .hover(|style| style.bg(cx.theme().secondary))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_2()
            .child(
                // Conversation title with optional cost below
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_1()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        if let Some(callback) = &on_click {
                            callback(&id_for_click, cx);
                        }
                    })
                    .child(
                        div()
                            .text_sm()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(self.is_collapsed, |d| d.child("•"))
                            .when(!self.is_collapsed, |d| d.child(self.title.clone())),
                    )
                    .when(!self.is_collapsed && self.cost_usd.is_some(), |parent| {
                        let cost = self.cost_usd.unwrap();
                        if cost <= 0.0 {
                            return parent;
                        }

                        // Always show in dollars for consistency
                        let cost_text = if cost >= 0.01 {
                            format!("${:.2}", cost)
                        } else if cost >= 0.001 {
                            format!("${:.3}", cost)
                        } else {
                            format!("${:.4}", cost)
                        };

                        parent.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(cost_text),
                        )
                    }),
            )
            .when(!self.is_collapsed && on_delete.is_some(), |this| {
                this.child(
                    // Delete button (visible on hover)
                    div()
                        .w_5()
                        .h_5()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .text_xs()
                        .text_color(rgb(0x666666))
                        .hover(|style| style.bg(rgb(0xef4444)).text_color(rgb(0xffffff)))
                        .child("×")
                        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                            cx.stop_propagation();
                            if let Some(callback) = &on_delete {
                                callback(&id_for_delete, cx);
                            }
                        }),
                )
            })
    }
}
