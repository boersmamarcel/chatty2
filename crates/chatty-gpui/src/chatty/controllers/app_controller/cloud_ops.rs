//! Cloud conversations on the desktop: opening one, following a turn that is
//! running there, answering what it waits on, and telling the user when it
//! finishes (HX-10, HX-11, HX-12).
//!
//! A cloud conversation is a synced conversation whose turns run on the
//! user's `chatty-server` ([`chatty_core::models::conversation::ConversationSync::runs_in_cloud`]). This device
//! holds a cache of it. Opening one therefore does three things, in this
//! order: bring the cache up to date when it is stale, load the approvals and
//! questions the run is waiting on, and follow the turn when one is running —
//! whoever started it.
//!
//! Following a turn is a stream like any other as far as `StreamManager`, the
//! transcript and the composer's queue can tell, but the local session never
//! opens a turn for it (see [`run_attach_stream`]): the server owns the turn
//! and its history, so when it ends the server's copy is adopted instead of
//! finalizing a reply locally.

use std::time::UNIX_EPOCH;

use chatty_core::models::clarification_store::ClarifyingQuestion;
use chatty_core::models::execution_approval_store::ApprovalDetail;
use chatty_core::services::hive_sync::{
    ApiError, CloudNotice, ConversationDetail, HiveConversationsApi, HttpHiveConversationsApi,
    TurnStatus, adopt_detail,
};
use chatty_core::session::{
    AttachOutcome, BrokerRequest, HostedFrame, InputOutcome, connector_settings_url,
};
use futures::StreamExt;

use super::message_ops_internals::{AttachParams, run_attach_stream};
use super::*;
use crate::chatty::services::os_notification;
use crate::chatty::views::ConnectorSignIn;
use crate::settings::models::GeneralSettingsModel;
use crate::settings::models::hive_settings::HiveSettingsModel;

use chatty_core::services::hive_sync::{ALREADY_ANSWERED, COULD_NOT_UPDATE, NO_LONGER_EXISTS};

impl ChattyApp {
    /// Route the frames cloud conversations send that are not session events
    /// (inputs the run waits on, connector sign-in) to the app, which owns the
    /// views they change.
    pub(super) fn start_cloud_frames(cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        cx.update_global::<ConversationsStore, _>(|store, _| store.set_frame_channel(tx));
        cx.spawn(async move |weak, cx| {
            while let Some((conv_id, frame)) = rx.next().await {
                if weak
                    .update(cx, |app, cx| app.handle_hosted_frame(&conv_id, frame, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn is_shown(&self, conv_id: &str, cx: &App) -> bool {
        self.chat_view
            .read(cx)
            .conversation_id()
            .is_some_and(|shown| shown == conv_id)
    }

    /// A frame from a cloud conversation's server: an approval or question to
    /// show, one that was settled, or a connector to sign in to (HX-11).
    pub(super) fn handle_hosted_frame(
        &mut self,
        conv_id: &str,
        frame: HostedFrame,
        cx: &mut Context<Self>,
    ) {
        match frame {
            HostedFrame::BrokerInput { id, nonce, request } => {
                if self.hive_sync.settled_inputs.contains(&id) {
                    return;
                }
                cx.global::<ConversationsStore>()
                    .remember_input(conv_id, &id, &nonce);
                if !self.is_shown(conv_id, cx) || self.chat_view.read(cx).knows_input(&id) {
                    return;
                }
                self.chat_view.update(cx, |view, cx| match request {
                    BrokerRequest::Approval(request) => {
                        // The asker is the card's title; every field the
                        // worker wrote is shown literally.
                        let detail = ApprovalDetail::forwarded(request);
                        view.handle_approval_requested(
                            id,
                            detail.description(),
                            false,
                            detail.asker,
                            cx,
                        );
                    }
                    BrokerRequest::Question(request) => {
                        view.handle_clarification_requested(
                            id,
                            ClarifyingQuestion::forwarded(request),
                            cx,
                        );
                    }
                });
            }
            HostedFrame::BrokerInputSettled { id, outcome } => {
                let approved = outcome == InputOutcome::Approved;
                self.settle_cloud_input(conv_id, &id, approved, None, cx);
            }
            HostedFrame::InputStale { id } => {
                self.settle_cloud_input(conv_id, &id, false, Some(ALREADY_ANSWERED), cx);
            }
            HostedFrame::ConnectorNeedsSignIn { connector } => {
                if !self.is_shown(conv_id, cx) {
                    return;
                }
                let url = connector_settings_url(&cx.global::<HiveSettingsModel>().hive_url);
                self.chat_view.update(cx, |view, cx| {
                    view.show_connector_sign_in(ConnectorSignIn { connector, url }, cx);
                });
            }
            HostedFrame::Session(_)
            | HostedFrame::Attached { .. }
            | HostedFrame::StreamError(_)
            | HostedFrame::Unknown(_) => {}
        }
    }

    /// An input is done with: its card goes, and a replay of its frame must
    /// not bring it back.
    fn settle_cloud_input(
        &mut self,
        conv_id: &str,
        id: &str,
        approved: bool,
        notice: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        cx.global::<ConversationsStore>().forget_input(id);
        self.hive_sync.settled_inputs.insert(id.to_string());
        if !self.is_shown(conv_id, cx) {
            return;
        }
        self.chat_view.update(cx, |view, cx| {
            view.settle_input(id, approved, cx);
            if let Some(notice) = notice {
                view.add_info_message(notice.to_string(), cx);
            }
        });
    }

    /// The address of `conv_id` on its server, when it runs in the cloud:
    /// `(server_url, remote_id, cache is stale)`.
    pub(super) fn cloud_address(&self, conv_id: &str, cx: &App) -> Option<(String, String, bool)> {
        let sync = cx
            .global::<ConversationsStore>()
            .get_conversation(conv_id)?
            .sync()
            .clone();
        let (server_url, remote_id) = sync.cloud_on()?;
        let stale = sync.synced()?.last_synced_at.is_none();
        Some((server_url.to_string(), remote_id.to_string(), stale))
    }

    /// A conversation was put on screen. When it runs in the cloud: bring the
    /// cache up to date if it is stale (which shows it again and lands back
    /// here), then load what its run waits on and follow the turn if one is
    /// running (HX-10, HX-11).
    pub(super) fn on_cloud_conversation_shown(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let Some((_, _, stale)) = self.cloud_address(conv_id, cx) else {
            return;
        };
        // Opened on this device: finishing or waiting is worth a notification.
        if self.hive_sync.engaged.insert(conv_id.to_string()) {
            self.hive_sync.save_notify_memory();
        }
        self.download_cloud_files(conv_id, cx);
        if stale {
            self.sync_cloud_detail(conv_id, cx);
            return;
        }
        self.load_cloud_inputs(conv_id, cx);
        self.attach_if_running(conv_id, cx);
    }

    /// Bring the conversation's input and output files that this device does
    /// not hold into the local cache (HX-15). The server deletes a run's
    /// outputs after 30 days unless they are promoted, so this runs when the
    /// conversation is opened, not when a file is first wanted.
    fn download_cloud_files(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let Some((server_url, remote_id, _)) = self.cloud_address(conv_id, cx) else {
            return;
        };
        let Some(dir) = chatty_core::services::hive_files::cache_dir(&remote_id) else {
            return;
        };
        if !self.hive_sync.files_in_flight.insert(conv_id.to_string()) {
            return;
        }
        let auth = cx.global::<ConversationsStore>().hosted_auth();
        let conv_id = conv_id.to_string();
        cx.spawn(async move |weak, cx| {
            let api = chatty_core::services::hive_files::HttpHiveFilesApi::new(&server_url, auth);
            let result =
                chatty_core::services::hive_files::download_files(&api, &remote_id, &dir).await;
            let outputs = match &result {
                Ok(_) => chatty_core::services::hive_files::cached_outputs(&api, &remote_id, &dir)
                    .await
                    .map_err(|error| {
                        warn!(error = %error, "Could not list a cloud run's outputs");
                    })
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            weak.update(cx, |app, cx| {
                app.hive_sync.files_in_flight.remove(&conv_id);
                app.show_cloud_outputs(&conv_id, &outputs, cx);
                match result {
                    Ok(files) => {
                        debug!(conv_id = %conv_id, count = files.len(), "Cached a cloud conversation's files");
                    }
                    Err(error) => {
                        warn!(conv_id = %conv_id, error = %error, "Could not cache a cloud conversation's files");
                    }
                }
            })
            .map_err(|e| debug!(error = ?e, "The app closed while caching cloud files"))
            .ok();
        })
        .detach();
    }

    /// Follow the conversation's turn when the server says one is running and
    /// this device is not already following it.
    pub(super) fn attach_if_running(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let store = cx.global::<ConversationsStore>();
        let busy = store.is_hosted(conv_id)
            && matches!(
                store.turn_status(conv_id),
                TurnStatus::Running | TurnStatus::WaitingForYou
            );
        let cooling = self
            .hive_sync
            .attach_failed_at
            .get(conv_id)
            .is_some_and(|at| at.elapsed() < super::hive_sync_ops::ATTACH_COOLDOWN);
        if busy && !cooling && !self.turn_is_active(conv_id, cx) {
            let accept_gap = self.hive_sync.accept_gap.remove(conv_id);
            self.attach_cloud_conversation(conv_id, accept_gap, cx);
        }
    }

    /// Fetch the conversation from the server and adopt it. Skipped while a
    /// fetch for it is already running.
    pub(super) fn sync_cloud_detail(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let Some((server_url, remote_id, _)) = self.cloud_address(conv_id, cx) else {
            return;
        };
        if !self.hive_sync.detail_in_flight.insert(conv_id.to_string()) {
            return;
        }
        let auth = cx.global::<ConversationsStore>().hosted_auth();
        let conv_id = conv_id.to_string();
        cx.spawn(async move |weak, cx| {
            let api = HttpHiveConversationsApi::new(&server_url, auth);
            let fetched = api.conversation(&remote_id).await;
            weak.update(cx, |app, cx| {
                app.hive_sync.detail_in_flight.remove(&conv_id);
                match fetched {
                    Ok(detail) => app.adopt_cloud_detail(&conv_id, detail, cx),
                    Err(error) => {
                        warn!(conv_id = %conv_id, error = %error, "Could not fetch a cloud conversation");
                        let notice = match error {
                            ApiError::SignedOut => error.to_string(),
                            ApiError::NotFound => NO_LONGER_EXISTS.to_string(),
                            _ => COULD_NOT_UPDATE.to_string(),
                        };
                        if app.is_shown(&conv_id, cx) {
                            app.chat_view
                                .update(cx, |view, cx| view.add_info_message(notice, cx));
                        }
                    }
                }
            })
            .map_err(|e| debug!(error = ?e, "The app closed while fetching a cloud conversation"))
            .ok();
        })
        .detach();
    }

    /// Replace this device's copy of a cloud conversation with the server's,
    /// remember the version it is at, and show it.
    fn adopt_cloud_detail(
        &mut self,
        conv_id: &str,
        detail: ConversationDetail,
        cx: &mut Context<Self>,
    ) {
        // A message the user sent while the turn was being followed starts the
        // next turn the moment this one ends. Replacing the history under it
        // would drop that message, so the server's copy waits: the conversation
        // stays marked stale and is fetched again when it is next opened.
        if self.turn_is_active(conv_id, cx) {
            debug!(conv_id = %conv_id, "A turn started; keeping the cloud copy for later");
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0);
        let adopted = cx.update_global::<ConversationsStore, _>(|store, _| {
            let Some(conversation) = store.get_conversation_mut(conv_id) else {
                return false;
            };
            let status = match adopt_detail(conversation, detail, now_ms) {
                Ok(status) => status,
                Err(error) => {
                    warn!(conv_id = %conv_id, error = ?error, "Could not adopt the server's history");
                    return false;
                }
            };
            let sync = conversation.sync().clone();
            store.set_conversation_sync(conv_id, sync);
            store.set_turn_status(conv_id, status);
            true
        });
        if !adopted {
            return;
        }
        self.persist_conversation(conv_id, cx);
        self.refresh_sidebar(cx);
        if self.active_conversation_id(cx).as_deref() == Some(conv_id) {
            // Shown again from the adopted copy; that lands in
            // `on_cloud_conversation_shown`, which now finds it fresh.
            self.display_loaded_conversation(conv_id, cx);
        }
    }

    /// Load the approvals and questions the conversation's run is waiting on.
    fn load_cloud_inputs(&mut self, conv_id: &str, cx: &mut Context<Self>) {
        let Some(fetch) = cx
            .global::<ConversationsStore>()
            .get_hosted(conv_id)
            .map(|hosted| hosted.fetch_inputs())
        else {
            return;
        };
        let conv_id = conv_id.to_string();
        cx.spawn(async move |weak, cx| match fetch.await {
            Ok(inputs) => {
                weak.update(cx, |app, cx| {
                    for input in inputs {
                        app.handle_hosted_frame(
                            &conv_id,
                            HostedFrame::BrokerInput {
                                id: input.id,
                                nonce: input.nonce,
                                request: input.request,
                            },
                            cx,
                        );
                    }
                })
                .map_err(|e| debug!(error = ?e, "The app closed while loading cloud inputs"))
                .ok();
            }
            Err(error) => {
                debug!(error = %error, "Could not load the inputs a cloud run waits on");
            }
        })
        .detach();
    }

    /// Follow the turn running on the server for `conv_id`, whoever started
    /// it. Does nothing while a stream for the conversation is already
    /// registered.
    pub(super) fn attach_cloud_conversation(
        &mut self,
        conv_id: &str,
        accept_gap: bool,
        cx: &mut Context<Self>,
    ) {
        if self.turn_is_active(conv_id, cx)
            || cx
                .global::<ConversationsStore>()
                .get_hosted(conv_id)
                .is_none()
        {
            return;
        }
        let Some(manager) = cx.try_global::<GlobalStreamManager>().and_then(|g| g.get()) else {
            warn!("StreamManager not available; cannot follow a cloud turn");
            return;
        };
        let conv_id = conv_id.to_string();
        let chat_view = self.chat_view.clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pending_artifacts = cx
            .global::<ConversationsStore>()
            .get_conversation(&conv_id)
            .map(|conversation| conversation.pending_artifacts());
        let task = {
            let conv_id = conv_id.clone();
            let cancel_flag = cancel_flag.clone();
            let stream_manager = Some(manager.clone());
            cx.spawn(async move |weak_ctrl, cx| -> anyhow::Result<()> {
                chat_view
                    .update(cx, |view, cx| view.start_assistant_message(cx))
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                run_attach_stream(
                    AttachParams {
                        conv_id,
                        chat_view,
                        stream_manager,
                        cancel_flag,
                        weak_ctrl,
                        accept_gap,
                    },
                    cx,
                )
                .await
            })
        };
        manager.update(cx, |mgr, cx| {
            mgr.register_stream(conv_id, task, cancel_flag, pending_artifacts, cx);
        });
    }

    /// A followed turn is over: record what became of it, then bring in the
    /// server's copy, which holds the reply.
    pub(super) fn finish_cloud_attach(
        &mut self,
        conv_id: &str,
        outcome: AttachOutcome,
        accept_gap: bool,
        cx: &mut Context<Self>,
    ) {
        if matches!(outcome, AttachOutcome::Lost | AttachOutcome::Busy) {
            self.hive_sync
                .attach_failed_at
                .insert(conv_id.to_string(), std::time::Instant::now());
        }
        cx.update_global::<ConversationsStore, _>(|store, _| match outcome {
            AttachOutcome::NeedsDetail => {}
            AttachOutcome::Lost => {
                // Not "running" any more as far as this device can tell; the
                // next pull says where it really stands and, when it is still
                // running, follows it again.
                store.set_turn_status(conv_id, TurnStatus::Idle);
                store.mark_cloud_failed(conv_id);
            }
            AttachOutcome::Ended | AttachOutcome::NotRunning | AttachOutcome::Busy => {
                store.set_turn_status(conv_id, TurnStatus::Idle);
            }
        });
        if outcome == AttachOutcome::NeedsDetail && !accept_gap {
            // The replay is missing the turn's start: read the conversation as
            // the server holds it, then follow the turn from where it is.
            self.hive_sync.accept_gap.insert(conv_id.to_string());
        }
        self.sync_cloud_detail(conv_id, cx);
    }

    /// Feed one pull's rows to the notification detector and show what it
    /// finds (HX-12).
    pub(super) fn notify_cloud_changes(
        &mut self,
        actions: &[chatty_core::services::hive_sync::SyncAction],
        cx: &mut Context<Self>,
    ) {
        use chatty_core::services::hive_sync::SyncAction;
        let mut notices = Vec::new();
        let mut detached = Vec::new();
        for action in actions {
            match action {
                SyncAction::UpsertMetadata(row) => {
                    let engaged = self.hive_sync.engaged.contains(&row.local_id);
                    notices.extend(self.hive_sync.detector.observe(
                        &row.local_id,
                        &row.title,
                        row.turn_status,
                        engaged,
                    ));
                }
                SyncAction::ApplyTombstone { local_id } => {
                    self.hive_sync.detector.forget(local_id);
                    self.hive_sync.engaged.remove(local_id);
                }
                SyncAction::DetachDeleted { local_id } => {
                    self.hive_sync.detector.forget(local_id);
                    self.hive_sync.engaged.remove(local_id);
                    detached.push(local_id.clone());
                }
                SyncAction::MarkDetailStale { .. } | SyncAction::FullResync => {}
            }
        }
        for notice in notices {
            self.show_cloud_notice(notice, cx);
        }
        // What the next start compares against (AGE-2127).
        if !actions.is_empty() {
            self.hive_sync.save_notify_memory();
        }
        // Deleted on the account while this device held it (AGE-2121): the
        // copy stays here, local-only, and the person hears it once, as when
        // a push meets the deletion (410).
        for local_id in detached {
            let title = cx
                .global::<ConversationsStore>()
                .metadata()
                .iter()
                .find(|row| row.id == local_id)
                .map(|row| row.title.clone())
                .unwrap_or_default();
            let message = if title.is_empty() {
                "A conversation was deleted on your Hive account. It stays on this device."
                    .to_string()
            } else {
                format!("\"{title}\" was deleted on your Hive account. It stays on this device.")
            };
            self.deleted_on_account(&local_id, message, cx);
        }
    }

    fn show_cloud_notice(&mut self, notice: CloudNotice, cx: &mut Context<Self>) {
        if !cx
            .global::<GeneralSettingsModel>()
            .notify_cloud_conversations
        {
            return;
        }
        // Looking at it right now: the transcript already says so.
        let watching = self.active_conversation_id(cx).as_deref() == Some(notice.local_id())
            && cx.active_window().is_some();
        if watching {
            return;
        }
        let text = notice.text();
        let conv_id = notice.local_id().to_string();
        cx.spawn(async move |weak, cx| {
            if os_notification::show("Chatty", &text).await {
                weak.update(cx, |app, cx| {
                    cx.activate(true);
                    app.load_conversation(&conv_id, cx);
                })
                .map_err(|e| debug!(error = ?e, "The app closed before a notification was clicked"))
                .ok();
            }
        })
        .detach();
    }
}
