use std::sync::Arc;

use crate::chatty::models::MessageFeedback;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

const BAR_HEIGHT: f32 = 28.0;

#[derive(IntoElement)]
pub struct MessageActionBar {
    message_id: String,
    content: String,
    feedback: Option<MessageFeedback>,
    always_visible: bool,
    on_feedback: Option<Arc<dyn Fn(Option<MessageFeedback>, &mut App) + 'static>>,
    on_regenerate: Option<Arc<dyn Fn(&mut App) + 'static>>,
}

impl MessageActionBar {
    pub fn new(message_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            content: content.into(),
            feedback: None,
            always_visible: false,
            on_feedback: None,
            on_regenerate: None,
        }
    }

    pub fn feedback(mut self, feedback: Option<MessageFeedback>) -> Self {
        self.feedback = feedback;
        self
    }

    pub fn always_visible(mut self, visible: bool) -> Self {
        self.always_visible = visible;
        self
    }

    pub fn on_feedback(mut self, f: impl Fn(Option<MessageFeedback>, &mut App) + 'static) -> Self {
        self.on_feedback = Some(Arc::new(f));
        self
    }

    pub fn on_regenerate(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_regenerate = Some(Arc::new(f));
        self
    }
}

impl RenderOnce for MessageActionBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let thumbs_up = matches!(self.feedback, Some(MessageFeedback::ThumbsUp));
        let thumbs_down = matches!(self.feedback, Some(MessageFeedback::ThumbsDown));
        let id = self.message_id.clone();
        let opacity = if self.always_visible { 1.0 } else { 0.0 };
        let on_feedback = self.on_feedback.clone();

        div()
            .id(ElementId::Name(format!("action-bar-{id}").into()))
            .h(px(BAR_HEIGHT))
            .flex()
            .flex_row()
            .justify_end()
            .items_center()
            .gap_1()
            .opacity(opacity)
            .hover(|s| s.opacity(1.0))
            .child(
                Button::new(ElementId::Name(format!("thumbs-up-{id}").into()))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::ThumbsUp).text_color(if thumbs_up {
                        cx.theme().success
                    } else {
                        muted
                    }))
                    .tooltip("Good response")
                    .on_click({
                        let on_feedback = on_feedback.clone();
                        let next = if thumbs_up {
                            None
                        } else {
                            Some(MessageFeedback::ThumbsUp)
                        };
                        move |_, _, cx| {
                            if let Some(cb) = &on_feedback {
                                cb(next.clone(), cx);
                            }
                        }
                    }),
            )
            .child(
                Popover::new(ElementId::Name(format!("thumbs-down-pop-{id}").into()))
                    .trigger(
                        Button::new(ElementId::Name(format!("thumbs-down-{id}").into()))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::ThumbsDown).text_color(if thumbs_down {
                                cx.theme().danger
                            } else {
                                muted
                            }))
                            .tooltip("Bad response"),
                    )
                    .content({
                        let on_feedback = on_feedback.clone();
                        move |_, _, cx| {
                            if let Some(cb) = &on_feedback {
                                cb(Some(MessageFeedback::ThumbsDown), cx);
                            }
                            div()
                                .p_2()
                                .text_xs()
                                .bg(cx.theme().popover)
                                .child("Thanks — we'll use this to improve.")
                        }
                    }),
            )
            .when_some(self.on_regenerate, |this, cb| {
                this.child(
                    Button::new(ElementId::Name(format!("regen-{id}").into()))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Redo).text_color(muted))
                        .tooltip("Regenerate")
                        .on_click(move |_, _, cx| cb(cx)),
                )
            })
            .child(Clipboard::new(ElementId::Name(format!("copy-{id}").into())).value(self.content))
    }
}
