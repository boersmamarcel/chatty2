//! What a cloud conversation offers the user once it has more to say than
//! the transcript: the files a run produced as cards, a conversation another
//! device holds ("Continue here"), and a turn the server interrupted
//! ("Resume") (HX-22).

use chatty_core::models::conversation::Placement;
use chatty_core::services::hive_files::{OutputFile, outputs_by_turn};
use chatty_core::services::hive_sync::{ApiError, HiveConversationsApi, HttpHiveConversationsApi};

use super::*;
use crate::chatty::views::FollowUp;
use crate::settings::controllers::extensions_controller::hive_session;
use crate::settings::models::hive_settings::HiveSettingsModel;
use chatty_core::session::{HandoffServer, this_device_id};

/// Shown when the server named neither a holder nor a message.
const HELD_ELSEWHERE: &str =
    "This conversation is running on another device. Continue it here first.";

/// What a conversation's follow-up line says, from what this device knows:
/// `None` when there is nothing to offer.
pub(super) fn follow_up_for(
    interrupted: bool,
    refused_push: Option<Option<String>>,
    placement: Option<&Placement>,
    this_device: Option<&str>,
    holder: Option<&str>,
) -> Option<FollowUp> {
    if interrupted {
        return Some(FollowUp::Resume);
    }
    let take_it_back = |message: Option<String>| FollowUp::TakeItBack {
        message: message.unwrap_or_else(|| HELD_ELSEWHERE.to_string()),
    };
    let held_here = matches!(
        (placement, this_device),
        (Some(Placement::Device { device_id }), Some(this)) if device_id == this
    );
    if let Some(message) = refused_push.filter(|_| !held_here) {
        return Some(take_it_back(message));
    }
    match (placement, this_device) {
        (Some(Placement::Device { device_id }), Some(this)) if device_id != this => {
            Some(take_it_back(holder.map(|name| {
                format!("This conversation is running on {name}. Continue it here first.")
            })))
        }
        _ => None,
    }
}

impl ChattyApp {
    /// Recompute the line under the shown conversation.
    pub(super) fn refresh_follow_up(&mut self, cx: &mut Context<Self>) {
        let Some(conv_id) = self.active_conversation_id(cx) else {
            return;
        };
        let store = cx.global::<ConversationsStore>();
        let interrupted = store.is_hosted(&conv_id)
            && store.turn_status(&conv_id)
                == chatty_core::services::hive_sync::TurnStatus::Interrupted;
        let placement = store
            .get_conversation(&conv_id)
            .and_then(|c| c.sync().synced().map(|s| s.placement.clone()));
        let this_device = store.this_device();
        let holder = store.device_name(&conv_id).map(str::to_string);
        if matches!(placement, Some(Placement::Device { .. })) {
            self.look_up_device_id(cx);
        }
        let follow_up = follow_up_for(
            interrupted,
            self.hive_push.held_message(&conv_id),
            placement.as_ref(),
            this_device.as_deref(),
            holder.as_deref(),
        );
        self.chat_view
            .update(cx, |view, cx| view.set_follow_up(follow_up, cx));
    }

    /// This device's id is otherwise only learned when a push goes out, but a
    /// pull that finds another device holding a conversation needs it to tell
    /// "held elsewhere" from "held here". Ask once, then redraw the line.
    fn look_up_device_id(&mut self, cx: &mut Context<Self>) {
        if cx.global::<ConversationsStore>().this_device().is_some()
            || self.hive_push.device_lookup_running()
        {
            return;
        }
        let settings = cx.global::<HiveSettingsModel>();
        if !settings.is_logged_in() {
            return;
        }
        let server_url = settings.hive_url.trim_end_matches('/').to_string();
        let Some(session) = hive_session(cx) else {
            return;
        };
        self.hive_push.set_device_lookup(true);
        cx.spawn(async move |weak, cx| {
            let device = this_device_id(&HandoffServer::new(&server_url, Some(session))).await;
            weak.update(cx, |app, cx| {
                app.hive_push.set_device_lookup(false);
                match device {
                    Ok(device) => {
                        cx.update_global::<ConversationsStore, _>(|store, _| {
                            store.set_this_device(Some(device));
                        });
                        app.refresh_follow_up(cx);
                    }
                    Err(error) => debug!(error = %error, "No Hive device id yet"),
                }
            })
            .map_err(|e| debug!(error = ?e, "The app closed during a device lookup"))
            .ok();
        })
        .detach();
    }

    /// Hang the files a cloud run produced on the turns they belong to.
    pub(super) fn show_cloud_outputs(
        &mut self,
        conv_id: &str,
        outputs: &[OutputFile],
        cx: &mut Context<Self>,
    ) {
        if !self.is_shown(conv_id, cx) {
            return;
        }
        let Some(conversation) = cx.global::<ConversationsStore>().get_conversation(conv_id) else {
            return;
        };
        let ends: Vec<(usize, Option<i64>)> = conversation
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                matches!(entry.message, rig_core::completion::Message::Assistant(_))
                    && !chatty_core::services::is_tool_message(&entry.message)
            })
            .map(|(ix, entry)| (ix, entry.timestamp))
            .collect();
        let by_turn = outputs_by_turn(outputs, &ends);
        self.chat_view
            .update(cx, |view, cx| view.set_cloud_outputs(by_turn, cx));
    }

    /// "Resume": ask the server to continue the interrupted turn, then follow
    /// it like any running one.
    pub(super) fn resume_turn(&mut self, cx: &mut Context<Self>) {
        let Some(conv_id) = self.active_conversation_id(cx) else {
            return;
        };
        let Some((server_url, remote_id, _)) = self.cloud_address(&conv_id, cx) else {
            return;
        };
        let auth = cx.global::<ConversationsStore>().hosted_auth();
        cx.spawn(async move |weak, cx| {
            let api = HttpHiveConversationsApi::new(&server_url, auth);
            let result = api.resume(&remote_id).await;
            weak.update(cx, |app, cx| match result {
                Ok(()) => {
                    cx.update_global::<ConversationsStore, _>(|store, _| {
                        store.set_turn_status(
                            &conv_id,
                            chatty_core::services::hive_sync::TurnStatus::Running,
                        );
                    });
                    app.refresh_follow_up(cx);
                    app.refresh_sidebar(cx);
                    app.attach_if_running(&conv_id, cx);
                }
                Err(error) => {
                    let notice = match error {
                        ApiError::Refused(_) | ApiError::SignedOut => error.to_string(),
                        _ => "Couldn't resume this turn. Try again in a moment.".to_string(),
                    };
                    warn!(conv_id = %conv_id, error = %error, "Could not resume a turn");
                    app.chat_view
                        .update(cx, |view, cx| view.add_info_message(notice, cx));
                }
            })
            .map_err(|e| debug!(error = ?e, "The app closed while resuming a turn"))
            .ok();
        })
        .detach();
    }

    /// "Continue here": take the conversation over from the device that
    /// holds it, through the same hand-off the move dialog uses.
    pub(super) fn continue_here(&mut self, cx: &mut Context<Self>) {
        if let Some(conv_id) = self.active_conversation_id(cx) {
            self.continue_conversation_here(&conv_id, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    // Explicit: `super::*` would pull in `gpui::test`, which shadows `#[test]`.
    use super::{FollowUp, HELD_ELSEWHERE, Placement, follow_up_for};

    fn device(id: &str) -> Placement {
        Placement::Device {
            device_id: id.to_string(),
        }
    }

    #[test]
    fn an_interrupted_turn_offers_resume_before_anything_else() {
        let held = Some(Some("held".to_string()));
        assert_eq!(
            follow_up_for(true, held, Some(&device("other")), Some("me"), None),
            Some(FollowUp::Resume)
        );
    }

    #[test]
    fn a_refused_push_shows_the_servers_own_words() {
        let message = "This conversation is running on Work laptop. Continue it here first.";
        assert_eq!(
            follow_up_for(
                false,
                Some(Some(message.to_string())),
                None,
                Some("me"),
                None
            ),
            Some(FollowUp::TakeItBack {
                message: message.to_string()
            })
        );
        // The server sent no message: the generic line.
        assert_eq!(
            follow_up_for(false, Some(None), None, None, None),
            Some(FollowUp::TakeItBack {
                message: HELD_ELSEWHERE.to_string()
            })
        );
    }

    #[test]
    fn a_pull_that_finds_another_device_holding_it_shows_the_banner() {
        assert_eq!(
            follow_up_for(false, None, Some(&device("laptop")), Some("me"), None),
            Some(FollowUp::TakeItBack {
                message: HELD_ELSEWHERE.to_string()
            })
        );
        // Held here, or this device's id not known yet: nothing to say.
        assert_eq!(
            follow_up_for(false, None, Some(&device("me")), Some("me"), None),
            None
        );
        assert_eq!(
            follow_up_for(false, None, Some(&device("laptop")), None, None),
            None
        );
        assert_eq!(follow_up_for(false, None, None, Some("me"), None), None);
    }

    #[test]
    fn the_banner_names_the_holder_and_goes_once_the_conversation_is_ours() {
        assert_eq!(
            follow_up_for(
                false,
                None,
                Some(&device("laptop")),
                Some("me"),
                Some("Work laptop")
            ),
            Some(FollowUp::TakeItBack {
                message: "This conversation is running on Work laptop. Continue it here first."
                    .to_string()
            })
        );
        // Taken back: the placement is this device's, so a refused push from
        // before no longer shows the banner.
        assert_eq!(
            follow_up_for(false, Some(None), Some(&device("me")), Some("me"), None),
            None
        );
    }
}
