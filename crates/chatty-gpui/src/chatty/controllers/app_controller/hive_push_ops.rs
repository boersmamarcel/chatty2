//! Pushing a device-held conversation to the Hive account after each turn
//! (AGE-2096, HX-17).
//!
//! A conversation that is synced with this device as its placement is the
//! server's copy's source of truth while it runs here, so after every turn its
//! history goes up with `If-Match: <version>`. The push is never part of the
//! turn: it is queued by the finalizer, runs on its own task, and a failure
//! only leaves it queued. The queue and the rules (412 pulls and never
//! overwrites, 409 stops pushing, offline waits in order) are
//! [`chatty_core::services::hive_sync::PushQueue`]; this file feeds it from the
//! store and carries its events out.
//!
//! A conversation that is not synced never gets this far: it is not enqueued,
//! and a queued one that turns out not to be held by this device is skipped
//! before any request is built.

use std::collections::HashMap;

use gpui_component::WindowExt;

use chatty_core::services::hive_sync::{
    HeldBy, HiveHistoryApi, HttpHiveHistoryApi, PushEvent, PushQueue, PushTarget, fetch_device_id,
    push_body,
};

use super::*;
use crate::settings::controllers::extensions_controller::hive_session;
use crate::settings::models::hive_settings::HiveSettingsModel;

/// A push that keeps hearing 412 after its pull gives up until the next turn,
/// so a server whose version never lines up cannot loop.
const MAX_STALE_RETRIES: u8 = 3;

/// What the app keeps between pushes.
#[derive(Default)]
pub(super) struct HivePushState {
    queue: PushQueue,
    in_flight: bool,
    /// Conversations that heard 412: pushed again once a pull has brought
    /// their version up to date, with how many times in a row.
    stale: HashMap<String, u8>,
    /// Conversations the server said another device holds, and who.
    held_by: HashMap<String, HeldBy>,
    /// This device's id on the server: the current login session, found once
    /// per sign-in and dropped when the user signs out.
    device: Option<String>,
}

/// One step of the push loop, read from the store.
enum Step {
    Done,
    Skip,
    Send {
        local_id: String,
        target: PushTarget,
        snapshot: Box<chatty_core::models::conversation::ConversationSnapshot>,
    },
}

impl ChattyApp {
    /// A turn of `conv_id` finished: queue its history for the account, if it
    /// is synced, and start pushing. A local-only conversation is not queued,
    /// so it makes no request.
    pub(super) fn push_after_turn(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let synced = cx
            .global::<ConversationsStore>()
            .get_conversation(conv_id)
            .is_some_and(|conversation| conversation.sync().synced().is_some());
        if !synced {
            return;
        }
        self.hive_push.queue.enqueue(conv_id);
        self.drive_hive_push(cx);
    }

    /// Push what is queued, one conversation at a time, until the queue is
    /// empty or a push cannot go. Also the retry: it runs on every sync tick
    /// and window activation, which is how an offline push goes out when the
    /// connection returns.
    pub(super) fn drive_hive_push(&mut self, cx: &mut Context<Self>) {
        if self.hive_push.in_flight || self.hive_push.queue.is_empty() {
            return;
        }
        let settings = cx.global::<HiveSettingsModel>();
        if !settings.is_logged_in() {
            self.hive_push.device = None;
            return;
        }
        let server_url = settings.hive_url.trim_end_matches('/').to_string();
        let Some(session) = hive_session(cx) else {
            return;
        };
        let cached_device = self.hive_push.device.clone();
        self.hive_push.in_flight = true;

        cx.spawn(async move |weak, cx| {
            let device = match cached_device {
                Some(device) => Some(device),
                None => match fetch_device_id(&server_url, Some(&session)).await {
                    Ok(device) => Some(device),
                    Err(error) => {
                        // Offline, or a sign-in that cannot hold a placement:
                        // nothing is held here to push, and the next tick asks
                        // again.
                        debug!(error = %error, "No Hive device id yet; not pushing");
                        None
                    }
                },
            };
            if let Some(device) = &device {
                weak.update(cx, |app, _| app.hive_push.device = Some(device.clone()))
                    .map_err(|e| debug!(error = ?e, "The app closed during a Hive push"))
                    .ok();
            }
            while let Some(device) = &device {
                let step = weak
                    .update(cx, |app, cx| app.next_push_step(device, cx))
                    .unwrap_or(Step::Done);
                let (local_id, target, snapshot) = match step {
                    Step::Done => break,
                    Step::Skip => continue,
                    Step::Send {
                        local_id,
                        target,
                        snapshot,
                    } => (local_id, target, snapshot),
                };
                // Serialized off the UI thread, like the row's own save.
                let body = cx
                    .background_executor()
                    .spawn(async move { snapshot.to_data().map(push_body) })
                    .await;
                let body = match body {
                    Ok(body) => body,
                    Err(error) => {
                        warn!(conv_id = %local_id, error = ?error, "Could not build a Hive push");
                        continue;
                    }
                };
                let api = HttpHiveHistoryApi::new(&target.server_url, Some(session.clone()));
                let result = api
                    .put_history(&target.remote_id, target.version, &body)
                    .await;
                let stop = weak
                    .update(cx, |app, cx| app.settle_push(&local_id, result, cx))
                    .unwrap_or(true);
                if stop {
                    break;
                }
            }
            weak.update(cx, |app, _| app.hive_push.in_flight = false)
                .map_err(|e| debug!(error = ?e, "The app closed during a Hive push"))
                .ok();
        })
        .detach();
    }

    /// Take the next queued conversation and read what its push needs. A
    /// conversation that is gone, mid-turn (its finalizer queues it again) or
    /// not held by this device is skipped without a request.
    fn next_push_step(&mut self, device: &str, cx: &mut Context<Self>) -> Step {
        let Some(local_id) = self.hive_push.queue.take() else {
            return Step::Done;
        };
        let store = cx.global::<ConversationsStore>();
        if store.is_streaming(&local_id) {
            return Step::Skip;
        }
        let Some(snapshot) = store
            .get_conversation(&local_id)
            .map(Conversation::snapshot)
        else {
            return Step::Skip;
        };
        match PushTarget::for_device(&snapshot.sync, device) {
            Some(target) => Step::Send {
                local_id,
                target,
                snapshot: Box::new(snapshot),
            },
            None => Step::Skip,
        }
    }

    /// Carry out how a push went. Returns whether the loop stops (offline or
    /// signed out: the push stays queued for the next tick).
    fn settle_push(
        &mut self,
        local_id: &str,
        result: Result<
            chatty_core::services::hive_sync::HistoryAccepted,
            chatty_core::services::hive_sync::PushError,
        >,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.hive_push.queue.settle(local_id, result) {
            PushEvent::Pushed { local_id, version } => {
                self.hive_push.stale.remove(&local_id);
                self.hive_push.held_by.remove(&local_id);
                let now_ms = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                cx.update_global::<ConversationsStore, _>(|store, _| {
                    let Some(mut synced) = store
                        .get_conversation(&local_id)
                        .and_then(|c| c.sync().synced().cloned())
                    else {
                        return;
                    };
                    synced.version = version;
                    synced.last_synced_at = Some(now_ms);
                    store.set_conversation_sync(
                        &local_id,
                        chatty_core::models::conversation::ConversationSync::Synced(synced),
                    );
                });
                self.persist_conversation(&local_id, cx);
                false
            }
            PushEvent::NeedsPull { local_id } => {
                // Never overwrite: pull, then push again if this device still
                // holds the conversation.
                let tries = self.hive_push.stale.entry(local_id.clone()).or_default();
                *tries += 1;
                if *tries > MAX_STALE_RETRIES {
                    warn!(conv_id = %local_id, "Hive keeps reporting a newer version; not pushing until the next turn");
                    self.hive_push.stale.remove(&local_id);
                } else {
                    self.pull_hive(cx);
                }
                false
            }
            PushEvent::HeldElsewhere { local_id, held_by } => {
                info!(conv_id = %local_id, "Another device holds this conversation; not pushing");
                cx.defer({
                    let message = held_by.continued_on();
                    move |cx| {
                        if let Some(window) = cx.active_window() {
                            window
                                .update(cx, |_, window, cx| window.push_notification(message, cx))
                                .map_err(|e| debug!(error = ?e, "No window for the held notice"))
                                .ok();
                        }
                    }
                });
                self.hive_push.held_by.insert(local_id, held_by);
                false
            }
            PushEvent::SignedOut | PushEvent::Offline => true,
            PushEvent::Dropped { local_id, reason } => {
                warn!(conv_id = %local_id, %reason, "The server refused a history push");
                false
            }
        }
    }

    /// A pull finished: the version of a conversation that heard 412 is up to
    /// date, so push it again if this device still holds it; and one that was
    /// held by another device but is ours again (taken back) may be pushed.
    pub(super) fn requeue_after_pull(&mut self, cx: &mut Context<Self>) {
        let Some(device) = self.hive_push.device.clone() else {
            return;
        };
        let held_here = |id: &str, cx: &Context<Self>| {
            cx.global::<ConversationsStore>()
                .get_conversation(id)
                .and_then(|c| PushTarget::for_device(c.sync(), &device))
                .is_some()
        };
        let stale: Vec<String> = self.hive_push.stale.keys().cloned().collect();
        for id in stale {
            if held_here(&id, cx) {
                self.hive_push.queue.enqueue(&id);
            } else {
                self.hive_push.stale.remove(&id);
            }
        }
        let held: Vec<String> = self.hive_push.held_by.keys().cloned().collect();
        for id in held {
            if held_here(&id, cx) {
                self.hive_push.held_by.remove(&id);
                self.hive_push.queue.release(&id);
                self.hive_push.queue.enqueue(&id);
            }
        }
        self.drive_hive_push(cx);
    }
}
