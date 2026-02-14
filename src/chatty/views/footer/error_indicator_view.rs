use crate::assets::CustomIcon;
use crate::chatty::models::ErrorStore;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme as _, Icon, Sizable, button::*, h_flex};

type ClickHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct ErrorIndicatorView {
    on_click: Option<ClickHandler>,
}

impl ErrorIndicatorView {
    pub fn new() -> Self {
        Self { on_click: None }
    }

    pub fn on_click<F>(mut self, handler: F) -> Self
    where
        F: Fn(&mut Window, &mut App) + 'static,
    {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ErrorIndicatorView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = cx.global::<ErrorStore>();
        let error_count = store.error_count();
        let warning_count = store.warning_count();
        let total = error_count + warning_count;

        // Use foreground color for visibility in both light/dark themes
        let text_color = cx.theme().foreground;

        // Background colors for the badge effect
        let error_bg = cx.theme().accent;
        let warning_bg = cx.theme().ring;

        div().when(total > 0, |this| {
            let mut button = Button::new("error-indicator")
                .ghost()
                .xsmall()
                .tooltip("View errors and warnings")
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Icon::new(CustomIcon::TriangleAlert)
                                .size(px(16.0))
                                .text_color(text_color),
                        )
                        // Show counts with colored backgrounds
                        .when(error_count > 0, |this| {
                            this.child(
                                div().px_1().rounded_sm().bg(error_bg).child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().background)
                                        .child(error_count.to_string()),
                                ),
                            )
                        })
                        .when(warning_count > 0, |this| {
                            this.child(
                                div().px_1().rounded_sm().bg(warning_bg).child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().background)
                                        .child(warning_count.to_string()),
                                ),
                            )
                        }),
                );

            if let Some(handler) = self.on_click {
                button = button.on_click(move |_event, window, cx| {
                    handler(window, cx);
                });
            }

            this.child(button)
        })
    }
}
