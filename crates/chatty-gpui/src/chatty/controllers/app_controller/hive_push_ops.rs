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
//! A conversation that is not synced is not queued, so it makes no request,
//! with one exception: a new conversation's first finished turn creates it on
//! the account while the user is signed in and "Sync new conversations" is on
//! (plan D1), as does the one-time "Sync my existing conversations" button.
//! Either way the create names the conversation by its local id, so a retry
//! after a lost reply finds the same one. "Keep on this device only" turns a
//! synced conversation back to local and leaves the account's copy alone.

use std::collections::HashMap;

use chatty_core::models::conversation::{ConversationSync, Placement, SyncedConversation};
use chatty_core::services::hive_sync::{
    ConflictBody, CreatedConversation, HistoryAccepted, HiveHistoryApi, HttpHiveHistoryApi,
    PushError, PushEvent, PushQueue, PushTarget, push_body,
};
use chatty_core::session::{HandoffServer, this_device_id};
use gpui_component::WindowExt;

use super::*;
use crate::settings::controllers::extensions_controller::{hive_session, save_hive_settings_async};
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
    held_by: HashMap<String, ConflictBody>,
    /// Local conversations whose next queue turn creates them on the account
    /// instead of pushing history.
    creates: std::collections::HashSet<String>,
    /// A lookup of this device's id is running (HX-22).
    device_lookup: bool,
}

/// Where a create gets the conversation from.
enum CreateFrom {
    /// Open in memory.
    Loaded(Box<chatty_core::models::conversation::ConversationSnapshot>),
    /// Only its saved row.
    Stored,
}

impl HivePushState {
    /// The server's words for a conversation another device holds, as of the
    /// last push it refused (HX-22).
    pub(super) fn held_message(&self, id: &str) -> Option<Option<String>> {
        self.held_by.get(id).map(|held| held.message.clone())
    }

    pub(super) fn set_device_lookup(&mut self, running: bool) {
        self.device_lookup = running;
    }

    /// Whether this device's id is being looked up now.
    pub(super) fn device_lookup_running(&self) -> bool {
        self.device_lookup
    }
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
    Create {
        local_id: String,
        from: CreateFrom,
    },
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl ChattyApp {
    /// A turn of `conv_id` finished: queue it for the account and start
    /// pushing. A synced conversation pushes its history; a local one is
    /// created on the account only when this was its first turn and new
    /// conversations sync; any other local conversation makes no request.
    pub(super) fn push_after_turn(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let Some((synced, first_turn)) = cx
            .global::<ConversationsStore>()
            .get_conversation(conv_id)
            .map(|conversation| {
                (
                    conversation.sync().synced().is_some(),
                    chatty_core::services::exchange_count(conversation.messages().iter()) == 1,
                )
            })
        else {
            return;
        };
        // A create still owed is still owed: a first turn the loop guard
        // follows up runs a second turn before the create could go (AGE-2124).
        let owed = self.hive_push.creates.contains(conv_id);
        if synced || owed {
            self.hive_push.queue.enqueue(conv_id);
        } else if first_turn && cx.global::<HiveSettingsModel>().syncs_new_conversations() {
            self.hive_push.creates.insert(conv_id.to_string());
            self.hive_push.queue.enqueue(conv_id);
        } else {
            return;
        }
        self.drive_hive_push(cx);
    }

    /// "Sync my existing conversations": every local conversation is created
    /// on the account, one at a time. Asked once; never done without the yes.
    pub(super) fn sync_existing_conversations(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global_mut::<HiveSettingsModel>();
        if !settings.is_logged_in() {
            return;
        }
        settings.sync_existing_offered = true;
        save_hive_settings_async(settings.clone(), cx);
        let local: Vec<String> = cx
            .global::<ConversationsStore>()
            .metadata()
            .iter()
            .filter(|row| row.sync.synced().is_none())
            .map(|row| row.id.clone())
            .collect();
        let count = local.len();
        for id in local {
            self.hive_push.creates.insert(id.clone());
            self.hive_push.queue.enqueue(&id);
        }
        self.notify_window(
            if count == 0 {
                "There are no conversations on this device to sync.".to_string()
            } else {
                format!(
                    "Syncing {count} conversations to your account. They appear on your other devices as they finish."
                )
            },
            cx,
        );
        self.drive_hive_push(cx);
    }

    /// "Keep on this device only": the conversation stops being pushed and
    /// becomes local-only. Its copy on the account is left alone.
    pub(super) fn keep_conversation_local(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        self.hive_push.queue.forget(conv_id);
        self.hive_push.creates.remove(conv_id);
        self.hive_push.stale.remove(conv_id);
        self.hive_push.held_by.remove(conv_id);
        cx.update_global::<ConversationsStore, _>(|store, _| {
            store.set_conversation_sync(conv_id, ConversationSync::LocalOnly);
        });
        self.persist_conversation(conv_id, cx);
        self.refresh_sidebar(cx);
        self.notify_window(
            "This conversation now stays on this device. Its copy on your account is untouched; delete it on the web if you no longer want it there."
                .to_string(),
            cx,
        );
    }

    pub(super) fn notify_window(&self, message: String, cx: &mut Context<Self>) {
        cx.defer(move |cx| {
            if let Some(window) = cx.active_window() {
                window
                    .update(cx, |_, window, cx| window.push_notification(message, cx))
                    .map_err(|e| debug!(error = ?e, "No window for a Hive notice"))
                    .ok();
            }
        });
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
            // This device's id belongs to the sign-in that just ended.
            cx.update_global::<ConversationsStore, _>(|store, _| store.set_this_device(None));
            return;
        }
        let server_url = settings.hive_url.trim_end_matches('/').to_string();
        let Some(session) = hive_session(cx) else {
            return;
        };
        // One cache per sign-in, shared with "Continue on this device".
        let cached_device = cx.global::<ConversationsStore>().this_device();
        let repo = self.conversation_repo.clone();
        self.hive_push.in_flight = true;

        cx.spawn(async move |weak, cx| {
            let device = match cached_device {
                Some(device) => Some(device),
                None => match this_device_id(&HandoffServer::new(&server_url, Some(session.clone())))
                    .await
                {
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
                weak.update(cx, |_, cx| {
                    cx.update_global::<ConversationsStore, _>(|store, _| {
                        store.set_this_device(Some(device.clone()))
                    })
                })
                    .map_err(|e| debug!(error = ?e, "The app closed during a Hive push"))
                    .ok();
            }
            while let Some(device) = &device {
                let step = weak
                    .update(cx, |app, cx| app.next_push_step(device, cx))
                    .unwrap_or(Step::Done);
                let stop = match step {
                    Step::Done => break,
                    Step::Skip => continue,
                    Step::Send {
                        local_id,
                        target,
                        snapshot,
                    } => {
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
                        let api =
                            HttpHiveHistoryApi::new(&target.server_url, Some(session.clone()));
                        let result = api
                            .put_history(&target.remote_id, target.version, &body)
                            .await;
                        weak.update(cx, |app, cx| app.settle_push(&local_id, result, cx))
                            .unwrap_or(true)
                    }
                    Step::Create { local_id, from } => {
                        let body = match from {
                            CreateFrom::Loaded(snapshot) => cx
                                .background_executor()
                                .spawn(async move { snapshot.to_data().map(push_body) })
                                .await
                                .map_err(|error| format!("{error:#}")),
                            CreateFrom::Stored => match repo.load_one(&local_id).await {
                                Ok(Some(row)) if row.sync.is_none() => Ok(push_body(row)),
                                Ok(_) => {
                                    // Gone, or synced meanwhile: nothing to create.
                                    weak.update(cx, |app, _| {
                                        app.hive_push.creates.remove(&local_id)
                                    })
                                    .ok();
                                    continue;
                                }
                                Err(error) => Err(format!("{error:#}")),
                            },
                        };
                        let body = match body {
                            Ok(body) => body,
                            Err(error) => {
                                warn!(conv_id = %local_id, %error, "Could not build a Hive create");
                                weak.update(cx, |app, _| app.hive_push.creates.remove(&local_id))
                                    .ok();
                                continue;
                            }
                        };
                        let api = HttpHiveHistoryApi::new(&server_url, Some(session.clone()));
                        let result = api.create(&local_id, &body).await;
                        let (server_url, device) = (server_url.clone(), device.clone());
                        weak.update(cx, |app, cx| {
                            app.settle_create(&local_id, result, &server_url, &device, cx)
                        })
                        .unwrap_or(true)
                    }
                };
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
            // Mid-turn: its finalizer queues it again, and a create it is
            // owed stays owed (`push_after_turn`).
            return Step::Skip;
        }
        let loaded = store
            .get_conversation(&local_id)
            .map(Conversation::snapshot);
        if self.hive_push.creates.contains(&local_id) {
            return match loaded {
                Some(snapshot) if snapshot.sync.synced().is_none() => Step::Create {
                    local_id,
                    from: CreateFrom::Loaded(Box::new(snapshot)),
                },
                Some(_) => {
                    // Synced meanwhile (a pull): push it like any other.
                    self.hive_push.creates.remove(&local_id);
                    self.hive_push.queue.enqueue(&local_id);
                    Step::Skip
                }
                None => Step::Create {
                    local_id,
                    from: CreateFrom::Stored,
                },
            };
        }
        let Some(snapshot) = loaded else {
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

    /// Carry out how a create went. The conversation becomes synced and held
    /// by this device, in memory and in its saved row.
    fn settle_create(
        &mut self,
        local_id: &str,
        result: Result<CreatedConversation, PushError>,
        server_url: &str,
        device: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let remote_id = result.as_ref().ok().map(|created| created.id.clone());
        let accepted = result.map(|created| HistoryAccepted {
            version: created.version,
        });
        match self.hive_push.queue.settle(local_id, accepted) {
            PushEvent::Pushed { local_id, version } => {
                self.hive_push.creates.remove(&local_id);
                let Some(remote_id) = remote_id else {
                    return false;
                };
                let sync = ConversationSync::Synced(SyncedConversation {
                    server_url: server_url.to_string(),
                    remote_id,
                    version,
                    placement: Placement::Device {
                        device_id: device.to_string(),
                    },
                    last_synced_at: Some(now_ms()),
                });
                let loaded = cx.update_global::<ConversationsStore, _>(|store, _| {
                    let loaded = store.get_conversation(&local_id).is_some();
                    store.set_conversation_sync(&local_id, sync.clone());
                    loaded
                });
                if loaded {
                    self.persist_conversation(&local_id, cx);
                } else {
                    let repo = self.conversation_repo.clone();
                    cx.spawn(async move |_, _| {
                        match repo.load_one(&local_id).await {
                            Ok(Some(mut row)) => {
                                let columns = sync.to_columns();
                                row.sync = columns.sync;
                                row.placement = columns.placement;
                                row.remote_version = columns.remote_version;
                                row.last_synced_at = columns.last_synced_at;
                                if let Err(error) = repo.save(&local_id, row).await {
                                    warn!(conv_id = %local_id, error = ?error, "Could not save a synced conversation");
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                warn!(conv_id = %local_id, error = ?error, "Could not read a synced conversation");
                            }
                        }
                    })
                    .detach();
                }
                self.refresh_sidebar(cx);
                false
            }
            PushEvent::SignedOut | PushEvent::Offline => true,
            PushEvent::Dropped { local_id, reason } => {
                warn!(conv_id = %local_id, %reason, "The server refused a new conversation");
                self.hive_push.creates.remove(&local_id);
                false
            }
            PushEvent::Deleted { local_id, message } => {
                self.hive_push.creates.remove(&local_id);
                self.deleted_on_account(&local_id, message, cx);
                false
            }
            PushEvent::NeedsPull { local_id } | PushEvent::HeldElsewhere { local_id, .. } => {
                // A create has no version to be stale and no holder to be
                // held by; the server answering so is not understood.
                warn!(conv_id = %local_id, "Unexpected answer to a create");
                self.hive_push.creates.remove(&local_id);
                false
            }
        }
    }

    /// Carry out how a push went. Returns whether the loop stops (offline or
    /// signed out: the push stays queued for the next tick).
    fn settle_push(
        &mut self,
        local_id: &str,
        result: Result<HistoryAccepted, PushError>,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.hive_push.queue.settle(local_id, result) {
            PushEvent::Pushed { local_id, version } => {
                self.hive_push.stale.remove(&local_id);
                self.hive_push.held_by.remove(&local_id);
                cx.update_global::<ConversationsStore, _>(|store, _| {
                    let Some(mut synced) = store
                        .get_conversation(&local_id)
                        .and_then(|c| c.sync().synced().cloned())
                    else {
                        return;
                    };
                    synced.version = version;
                    synced.last_synced_at = Some(now_ms());
                    store.set_conversation_sync(&local_id, ConversationSync::Synced(synced));
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
                // The server's own words, as they are.
                self.notify_window(held_by.notice(), cx);
                self.hive_push.held_by.insert(local_id, held_by);
                self.refresh_follow_up(cx);
                false
            }
            PushEvent::SignedOut | PushEvent::Offline => true,
            PushEvent::Dropped { local_id, reason } => {
                warn!(conv_id = %local_id, %reason, "The server refused a history push");
                false
            }
            PushEvent::Deleted { local_id, message } => {
                self.deleted_on_account(&local_id, message, cx);
                false
            }
        }
    }

    /// 410: the conversation was deleted on the account and its id stays
    /// retired. It becomes local-only so nothing retries; the local copy is
    /// kept, never deleted; the server's message is shown once.
    pub(super) fn deleted_on_account(
        &mut self,
        local_id: &str,
        message: String,
        cx: &mut Context<Self>,
    ) {
        info!(conv_id = %local_id, "Deleted on the Hive account; keeping it on this device");
        self.hive_push.queue.forget(local_id);
        self.hive_push.stale.remove(local_id);
        self.hive_push.held_by.remove(local_id);
        let loaded = cx.update_global::<ConversationsStore, _>(|store, _| {
            store.set_conversation_sync(local_id, ConversationSync::LocalOnly);
            store.get_conversation(local_id).is_some()
        });
        if loaded {
            self.persist_conversation(local_id, cx);
        }
        self.refresh_sidebar(cx);
        self.notify_window(message, cx);
    }

    /// A pull finished: the version of a conversation that heard 412 is up to
    /// date, so push it again if this device still holds it; and one that was
    /// held by another device but is ours again (taken back) may be pushed.
    pub(super) fn requeue_after_pull(&mut self, cx: &mut Context<Self>) {
        let Some(device) = cx.global::<ConversationsStore>().this_device() else {
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
