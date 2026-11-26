//! Pulling the conversation list from the user's Hive account (AGE-2088).
//!
//! While signed in, the app pulls once the local rows have loaded, on every
//! window activation, and every [`PULL_INTERVAL`] while it is open. A laptop
//! that wakes from sleep fires the overdue interval timer at once, so a wake
//! is a pull too. There is no background process: a closed app pulls nothing.
//!
//! The pull only *reads* the server. What it changes is this device's cache:
//! the sidebar rows, the loaded conversations' sync state, and the persisted
//! rows. The sidebar shows each cloud conversation's live status (HX-10), a
//! turn that started elsewhere is followed (`cloud_ops`), and a finish or a
//! wait the user should hear about becomes a notification (HX-12).

use std::collections::HashSet;

use chatty_core::services::hive_sync::{
    self, HttpHiveConversationsApi, PULL_INTERVAL, SyncEngine, TransitionDetector,
};

use super::*;
use crate::settings::controllers::extensions_controller::hive_session;
use crate::settings::models::hive_settings::HiveSettingsModel;

/// How long a failed follow of a running turn waits before the next try; a
/// pull after that retries it.
pub(super) const ATTACH_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// What the app keeps between pulls.
#[derive(Default)]
pub(super) struct HiveSyncState {
    /// Owns the cursor; rebuilt (a full pull) when the Hive URL changes.
    engine: Option<SyncEngine>,
    in_flight: bool,
    /// The local rows have loaded. A pull before that would not match the
    /// server's conversations against them and would create duplicates.
    rows_loaded: bool,
    /// The last failure, so a server that keeps failing is logged once per
    /// distinct error rather than every interval.
    last_error: Option<String>,
    /// Cloud conversations this device opened: only these notify (HX-12).
    pub(super) engaged: HashSet<String>,
    /// Remembers each conversation's last status, so only a change notifies.
    pub(super) detector: TransitionDetector,
    /// Inputs a cloud run waited on that are done, so a replayed frame does
    /// not bring their cards back (HX-11).
    pub(super) settled_inputs: HashSet<String>,
    /// Conversations whose next follow of the running turn accepts a replay
    /// with dropped frames, because their detail was just fetched (HX-10).
    pub(super) accept_gap: HashSet<String>,
    /// Conversations whose detail is being fetched.
    pub(super) detail_in_flight: HashSet<String>,
    /// Conversations whose files are being brought into the local cache
    /// (HX-15).
    pub(super) files_in_flight: HashSet<String>,
    /// When following a conversation's turn last failed (the connection was
    /// lost, or the server had no room for another viewer). It is not
    /// followed again for [`ATTACH_COOLDOWN`]: the server's status can say
    /// "running" the whole time, and retrying at once would spin.
    pub(super) attach_failed_at: std::collections::HashMap<String, std::time::Instant>,
}

impl ChattyApp {
    /// Pull on every window activation and every [`PULL_INTERVAL`].
    pub(super) fn start_hive_sync(window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_window_activation(window, |app, window, cx| {
            if window.is_window_active() {
                app.pull_hive(cx);
                app.drive_hive_push(cx);
            }
        })
        .detach();
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(PULL_INTERVAL).await;
                if weak
                    .update(cx, |app, cx| {
                        app.pull_hive(cx);
                        app.drive_hive_push(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// The local rows are in the store: pulls may start, and the first one
    /// runs now.
    pub(super) fn hive_rows_loaded(&mut self, cx: &mut Context<Self>) {
        self.hive_sync.rows_loaded = true;
        self.pull_hive(cx);
    }

    /// Pull once, unless one is already running or nobody is signed in.
    pub(super) fn pull_hive(&mut self, cx: &mut Context<Self>) {
        if !self.hive_sync.rows_loaded || self.hive_sync.in_flight {
            return;
        }
        let settings = cx.global::<HiveSettingsModel>();
        if !settings.is_logged_in() {
            return;
        }
        let Some(session) = hive_session(cx) else {
            return;
        };
        // One Hive URL per account (plan D5, AGE-2085): `hive_url` is the
        // chatty-server's origin.
        let server_url = settings.hive_url.trim_end_matches('/').to_string();
        let mut engine = match self.hive_sync.engine.take() {
            Some(engine) if engine.server_url() == server_url => engine,
            _ => SyncEngine::new(&server_url),
        };
        let local = cx.global::<ConversationsStore>().metadata().to_vec();
        let repo = self.conversation_repo.clone();
        self.hive_sync.in_flight = true;

        cx.spawn(async move |weak, cx| {
            let api = HttpHiveConversationsApi::new(&server_url, Some(session));
            let result = hive_sync::pull(&api, &mut engine, &local).await;
            let to_persist = weak
                .update(cx, |app, cx| {
                    app.hive_sync.in_flight = false;
                    app.hive_sync.engine = Some(engine);
                    match result {
                        Ok(actions) => {
                            app.hive_sync.last_error = None;
                            let kept = if actions.is_empty() {
                                Vec::new()
                            } else {
                                debug!(count = actions.len(), "Applying a Hive sync pull");
                                let kept = cx.update_global::<ConversationsStore, _>(|store, _| {
                                    store.apply_sync_actions(&actions)
                                });
                                app.refresh_sidebar(cx);
                                kept
                            };
                            app.notify_cloud_changes(&actions, cx);
                            app.requeue_after_pull(cx);
                            app.refresh_follow_up(cx);
                            // A turn started elsewhere while this conversation
                            // is open, or one whose connection a sleep cut,
                            // is followed again.
                            if let Some(active) = app.active_conversation_id(cx) {
                                app.attach_if_running(&active, cx);
                            }
                            kept
                        }
                        Err(error) => {
                            let message = error.to_string();
                            if app.hive_sync.last_error.as_deref() != Some(message.as_str()) {
                                warn!(error = %message, "Hive sync pull failed");
                                app.hive_sync.last_error = Some(message);
                            }
                            Vec::new()
                        }
                    }
                })
                .map_err(|e| debug!(error = ?e, "The app closed during a Hive sync pull"))
                .unwrap_or_default();
            if !to_persist.is_empty()
                && let Err(error) = hive_sync::apply_to_repository(&*repo, &to_persist).await
            {
                warn!(error = ?error, "Failed to persist a Hive sync pull");
            }
        })
        .detach();
    }
}
