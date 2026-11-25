//! The confirmation for taking a conversation online, or bringing it back
//! (AGE-298).
//!
//! Taking a conversation online is a **data egress**: its whole history leaves
//! this machine. So the dialog's job is not to ask "are you sure" — it is to
//! say, before the user agrees, exactly what travels and exactly what does
//! not. The two lists come from chatty-core's `MoveSummary`, the same constant
//! the TUI's `/online` prints, so the desktop and the terminal cannot end up
//! telling the user different things about what leaves their machine.
//!
//! The dialog does not perform the move; it collects the decision and hands
//! the account's Hive address (AGE-2085: one address, no field to type)
//! back to the controller, which owns the ordering that makes a
//! failed move harmless.

use chatty_core::session::{BRING_BACK_SUMMARY, MoveSummary, TAKE_ONLINE_SUMMARY};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    v_flex,
};

use crate::chatty::controllers::GlobalChattyApp;

/// The dialog's body: what moves and what does not.
pub struct MoveConversationView {
    conv_id: String,
    /// `true` when the conversation is local and this is the trip out.
    going_online: bool,
}

impl MoveConversationView {
    pub fn new(
        conv_id: String,
        going_online: bool,
    ) -> Self {
        Self {
            conv_id,
            going_online,
        }
    }

    fn summary(&self) -> &'static MoveSummary {
        if self.going_online {
            &TAKE_ONLINE_SUMMARY
        } else {
            &BRING_BACK_SUMMARY
        }
    }
}

impl Render for MoveConversationView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = self.summary();
        let going_online = self.going_online;
        let conv_id = self.conv_id.clone();
        let hive = cx.global::<HiveSettingsModel>();
        let hive_url = hive.hive_url.clone();
        let signed_in = hive.is_logged_in();
        // Going online needs the account; bringing one back needs it too, but
        // that path already says so itself when the server answers 401.
        let blocked = going_online && !signed_in;

        v_flex()
            .gap_4()
            .p_2()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if going_online {
                        format!("This conversation's history will be uploaded to your Hive account at {hive_url} and will run there until you bring it back. It stays on this machine too.")
                    } else {
                        "This conversation will run on this machine again. Anything it did online that was not part of the conversation stays there.".to_string()
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("What moves"),
                    )
                    .children(summary.moves.iter().map(|item| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(div().text_xs().text_color(cx.theme().green).child("+"))
                            .child(div().text_xs().child(*item))
                    })),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("What does not move"),
                    )
                    .children(summary.does_not_move.iter().map(|(what, why)| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("−"),
                            )
                            .child(
                                v_flex()
                                    .child(div().text_xs().child(*what))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(*why),
                                    ),
                            )
                    })),
            )
            .when(blocked, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child("You are not signed in to Hive. Sign in under Settings → Extensions → Hive account, then try again."),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-move")
                            .label("Cancel")
                            .small()
                            .ghost()
                            .on_click(|_event, window, cx| {
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("confirm-move")
                            .label(if going_online {
                                "Take online"
                            } else {
                                "Bring back"
                            })
                            .small()
                            .primary()
                            .disabled(blocked)
                            .on_click(move |_event, window, cx| {
                                let target = going_online.then(|| hive_url.clone());
                                window.close_dialog(cx);
                                if let Some(app) = cx
                                    .try_global::<GlobalChattyApp>()
                                    .and_then(|g| g.try_upgrade())
                                {
                                    let conv_id = conv_id.clone();
                                    app.update(cx, |app, cx| {
                                        app.move_conversation(&conv_id, target, cx);
                                    });
                                }
                            }),
                    ),
            )
    }
}

pub struct MoveConversationDialog;

impl MoveConversationDialog {
    pub fn open(conv_id: String, going_online: bool, window: &mut Window, cx: &mut App) {
        let view = cx.new(|_cx| MoveConversationView::new(conv_id, going_online));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title(if going_online {
                    "Take this conversation online"
                } else {
                    "Bring this conversation back"
                })
                .w(px(520.))
                .child(view.clone())
        });
    }
}
