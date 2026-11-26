//! The confirmation for "Send to the cloud" and "Continue on this device"
//! (HX-14, wording HX-16).
//!
//! Sending a conversation to the cloud takes its history off this computer,
//! so the dialog's job is to say, before the user agrees, what goes with it
//! and what stays. The lists come from chatty-core's `HandoffSummary`, which
//! the TUI prints too. When the conversation's model only runs on this
//! computer, the dialog asks for a cloud model with the account's default
//! preselected.
//!
//! The dialog performs nothing itself: it hands the decision to the
//! controller, which owns the ordering that makes a failed hand-off harmless.

use chatty_core::services::hive_sync::CloudModels;
use chatty_core::session::{
    CONTINUE_HERE_SUMMARY, HandoffServer, HandoffSummary, LOCAL_MODEL_NOTE, SEND_TO_CLOUD_SUMMARY,
    cloud_models,
};
use chatty_core::settings::models::hive_settings::HiveSettingsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};
use tracing::warn;

use crate::chatty::controllers::GlobalChattyApp;
use crate::chatty::models::ConversationsStore;

/// Which way the conversation goes.
#[derive(Clone, Debug)]
pub enum HandoffKind {
    SendToCloud,
    ContinueHere,
    /// A send was refused because another device or the cloud holds the
    /// conversation; `message` says which. The way on is to take it over.
    TakeOver {
        message: String,
    },
}

/// The account's cloud models, as far as the dialog knows them.
enum Roster {
    Loading,
    Loaded(CloudModels),
    Failed(String),
}

/// The dialog's body.
pub struct HandoffView {
    conv_id: String,
    kind: HandoffKind,
    /// The conversation's own model.
    local_model: String,
    roster: Roster,
    picked: Option<String>,
}

impl HandoffView {
    fn new(
        conv_id: String,
        kind: HandoffKind,
        local_model: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            conv_id,
            kind,
            local_model,
            roster: Roster::Loading,
            picked: None,
        };
        if matches!(view.kind, HandoffKind::SendToCloud) {
            view.load_roster(cx);
        }
        view
    }

    fn load_roster(&mut self, cx: &mut Context<Self>) {
        let hive_url = cx.global::<HiveSettingsModel>().hive_url.clone();
        let auth = cx.global::<ConversationsStore>().hosted_auth();
        cx.spawn(async move |this, cx| {
            let result = cloud_models(&HandoffServer::new(&hive_url, auth)).await;
            this.update(cx, |view, cx| {
                view.roster = match result {
                    Ok(models) => {
                        view.picked = models
                            .preselected(&view.local_model)
                            .map(|model| model.id.clone());
                        Roster::Loaded(models)
                    }
                    Err(error) => Roster::Failed(error.to_string()),
                };
                cx.notify();
            })
            .map_err(|e| warn!(error = ?e, "The hand-off dialog closed before the models loaded"))
            .ok();
        })
        .detach();
    }

    fn summary(&self) -> &'static HandoffSummary {
        match self.kind {
            HandoffKind::SendToCloud => &SEND_TO_CLOUD_SUMMARY,
            HandoffKind::ContinueHere | HandoffKind::TakeOver { .. } => &CONTINUE_HERE_SUMMARY,
        }
    }

    /// Does the conversation's own model run in the cloud? `None` while the
    /// roster is not known.
    fn local_model_runs_in_cloud(&self) -> Option<bool> {
        match &self.roster {
            Roster::Loaded(models) => Some(models.runs(&self.local_model)),
            _ => None,
        }
    }

    fn list(
        title: &'static str,
        items: &'static [&'static str],
        mark: &'static str,
        cx: &App,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(title),
            )
            .children(items.iter().map(|item| {
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(mark),
                    )
                    .child(div().flex_1().min_w_0().text_xs().child(*item))
            }))
    }

    fn model_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        match &self.roster {
            Roster::Loading => div()
                .text_xs()
                .text_color(muted)
                .child("Checking which models your Hive account runs…")
                .into_any_element(),
            Roster::Failed(message) => div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(message.clone())
                .into_any_element(),
            Roster::Loaded(models) if models.models.is_empty() => div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child("Your Hive account has no cloud models yet. Add one in your Hive account's settings, then try again.")
                .into_any_element(),
            Roster::Loaded(models) if models.runs(&self.local_model) => {
                let name = models
                    .models
                    .iter()
                    .find(|model| model.id == self.local_model)
                    .map(|model| model.name.clone())
                    .unwrap_or_else(|| self.local_model.clone());
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("In the cloud it keeps using {name}."))
                    .into_any_element()
            }
            Roster::Loaded(models) => v_flex()
                .gap_2()
                .child(div().text_xs().child(LOCAL_MODEL_NOTE))
                .child(
                    h_flex().flex_wrap().gap_1().children(models.models.iter().map(|model| {
                        let id = model.id.clone();
                        let picked = self.picked.as_deref() == Some(model.id.as_str());
                        let button = Button::new(SharedString::from(format!("cloud-model-{}", model.id)))
                            .label(model.name.clone())
                            .xsmall()
                            .when(picked, |button| button.icon(Icon::new(IconName::Check)))
                            .on_click(cx.listener(move |view, _event, _window, cx| {
                                view.picked = Some(id.clone());
                                cx.notify();
                            }));
                        if picked { button.primary() } else { button.outline() }
                    })),
                )
                .into_any_element(),
        }
    }
}

impl Render for HandoffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = self.summary();
        let kind = self.kind.clone();
        let conv_id = self.conv_id.clone();
        let signed_in = cx.global::<HiveSettingsModel>().is_logged_in();
        let to_cloud = matches!(kind, HandoffKind::SendToCloud);
        let ready = !to_cloud
            || match self.local_model_runs_in_cloud() {
                Some(true) => true,
                Some(false) => self.picked.is_some(),
                None => false,
            };
        let blocked = !signed_in || !ready;
        // Sent only when the conversation's own model does not run there.
        let cloud_model = match self.local_model_runs_in_cloud() {
            Some(false) => self.picked.clone(),
            _ => None,
        };

        v_flex()
            .gap_4()
            .p_2()
            .when_some(
                match &kind {
                    HandoffKind::TakeOver { message } => Some(message.clone()),
                    _ => None,
                },
                |this, message| this.child(div().text_sm().child(message)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary.lead),
            )
            .child(
                h_flex()
                    .gap_6()
                    .items_start()
                    .child(Self::list(summary.goes_title, summary.goes, "+", cx))
                    .child(Self::list(summary.stays_title, summary.stays, "−", cx)),
            )
            .when(to_cloud, |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Model"),
                        )
                        .child(self.model_section(cx)),
                )
            })
            .when(!signed_in, |this| {
                this.child(div().text_xs().text_color(cx.theme().danger).child(
                    "Sign in to your Hive account first: Settings → Extensions → Hive account.",
                ))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-handoff")
                            .label("Cancel")
                            .small()
                            .ghost()
                            .on_click(|_event, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-handoff")
                            .label(if to_cloud {
                                "Send to the cloud"
                            } else {
                                "Continue on this device"
                            })
                            .small()
                            .primary()
                            .disabled(blocked)
                            .on_click(move |_event, window, cx| {
                                window.close_dialog(cx);
                                let Some(app) = cx
                                    .try_global::<GlobalChattyApp>()
                                    .and_then(|g| g.try_upgrade())
                                else {
                                    return;
                                };
                                let conv_id = conv_id.clone();
                                let cloud_model = cloud_model.clone();
                                app.update(cx, |app, cx| {
                                    if to_cloud {
                                        app.send_conversation_to_cloud(&conv_id, cloud_model, cx);
                                    } else {
                                        app.continue_conversation_here(&conv_id, cx);
                                    }
                                });
                            }),
                    ),
            )
    }
}

pub struct HandoffDialog;

impl HandoffDialog {
    pub fn open(
        conv_id: String,
        kind: HandoffKind,
        local_model: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let title = match kind {
            HandoffKind::SendToCloud => "Send this conversation to the cloud",
            HandoffKind::ContinueHere => "Continue this conversation on this device",
            HandoffKind::TakeOver { .. } => "This conversation is open somewhere else",
        };
        let view = cx.new(|cx| HandoffView::new(conv_id, kind, local_model, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog.title(title).w(px(560.)).child(view.clone())
        });
    }
}
