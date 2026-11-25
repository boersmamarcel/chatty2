//! Pulling the conversation list from the user's Hive account (AGE-2088).
//!
//! While signed in (and, until HX-10's sidebar lands, with the hosted
//! conversations developer switch on), the app pulls once the local rows have loaded, on every
//! window activation, and every [`PULL_INTERVAL`] while it is open. A laptop
//! that wakes from sleep fires the overdue interval timer at once, so a wake
//! is a pull too. There is no background process: a closed app pulls nothing.
//!
//! The pull only *reads* the server. What it changes is this device's cache:
//! the sidebar rows, the loaded conversations' sync state, and the persisted
//! rows. Showing the result (badges, live status) is HX-10.

use chatty_core::services::hive_sync::{self, HttpHiveConversationsApi, PULL_INTERVAL, SyncEngine};

use super::*;
use crate::settings::controllers::extensions_controller::hive_session;
use crate::settings::models::hive_settings::HiveSettingsModel;

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
}

impl ChattyApp {
    /// Pull on every window activation and every [`PULL_INTERVAL`].
    pub(super) fn start_hive_sync(window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_window_activation(window, |app, window, cx| {
            if window.is_window_active() {
                app.pull_hive(cx);
            }
        })
        .detach();
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(PULL_INTERVAL).await;
                if weak.update(cx, |app, cx| app.pull_hive(cx)).is_err() {
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
        // Behind the same developer switch as the move until the sidebar can
        // show a synced conversation and fetch its history (HX-10): a pulled
        // row has no history until then.
        if !super::conversation_ops_modify::move_ui_enabled(cx) {
            return;
        }
        let settings = cx.global::<HiveSettingsModel>();
        if !settings.is_logged_in() {
            return;
        }
        let Some(session) = hive_session(cx) else {
            return;
        };
        // One Hive URL per account (plan D5): the registry's origin is the
        // chatty-server's.
        let server_url = settings.registry_url.trim_end_matches('/').to_string();
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
                            if actions.is_empty() {
                                return Vec::new();
                            }
                            debug!(count = actions.len(), "Applying a Hive sync pull");
                            let kept = cx.update_global::<ConversationsStore, _>(|store, _| {
                                store.apply_sync_actions(&actions)
                            });
                            app.refresh_sidebar(cx);
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
