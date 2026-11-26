//! Synced and cloud conversations in the terminal (HX-19): listing them,
//! opening one, following a turn that runs on the server, sending to it and
//! answering what it waits on.
//!
//! The terminal does what the desktop's `cloud_ops.rs` does, over the same
//! pieces in chatty-core: the sync engine and repository for the list, the
//! detail fetch (`adopt_detail`) for opening, `HostedSession::attach` to
//! follow a running turn, and the hosted input routes for approvals and
//! questions a cloud run raises. What is terminal-only is the glue: there is
//! no background pull, so `/conversations` pulls, and a followed turn is
//! drawn from the event stream and then replaced by the server's copy.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, anyhow};
use chatty_core::models::clarification_store::{ClarificationAnswer, ClarifyingQuestion};
use chatty_core::models::execution_approval_store::ApprovalDetail;
use chatty_core::repositories::{ConversationRepository, ConversationSqliteRepository};
use chatty_core::services::hive_sync::{
    ALREADY_ANSWERED, ApiError, COULD_NOT_UPDATE, ConversationDetail, HiveConversationsApi,
    HttpHiveConversationsApi, NO_LONGER_EXISTS, SyncAction, SyncEngine, TurnStatus, adopt_detail,
    apply_to_repository, pull,
};
use chatty_core::session::{
    AnswerOutcome, AttachOutcome, BrokerRequest, HostedFrame, HostedSession, InputAnswer,
    InputOutcome, connector_settings_url,
};

use super::{ChatEngine, PendingApproval, PendingClarification};
use crate::events::AppEvent;
use crate::ui::conversations;

/// What the terminal keeps about the cloud between commands.
#[derive(Default)]
pub(super) struct CloudState {
    repo: Option<Arc<dyn ConversationRepository>>,
    api: Option<Arc<dyn HiveConversationsApi>>,
    sync: Option<SyncEngine>,
    pub(super) hive_url: Option<String>,
    /// The Hive sign-in the cloud requests authenticate with.
    pub(super) auth: Option<Arc<chatty_core::hive::HiveSession>>,
    /// The device holding each device-placed conversation, by name, as of
    /// the last pull.
    held_by: HashMap<String, String>,
    /// This device's id on the server, once known.
    pub(super) this_device: Option<String>,
    /// Each conversation's turn as of the last pull or fetch, by local id.
    status: HashMap<String, TurnStatus>,
    /// The ids of the last `/conversations` list, which `/open <n>` counts in.
    listing: Vec<String>,
    /// The nonce each pending cloud input's answer has to carry.
    nonces: HashMap<String, String>,
    /// Inputs already answered or withdrawn, which a replay must not raise
    /// again.
    settled: HashSet<String>,
    /// A cloud turn is being followed: the server owns it, so nothing of it
    /// is finalized locally.
    following: bool,
    /// The replay of the turn is missing its start: follow it anyway once
    /// the conversation has been read.
    accept_gap: bool,
    /// Following failed (lost or busy): do not retry until it is reopened.
    attach_failed: bool,
    detail_in_flight: bool,
}

type Answer = Pin<Box<dyn Future<Output = ()> + Send>>;

impl ChatEngine {
    /// Use `repo` and `api` for the cloud commands instead of the user's
    /// saved ones: what the characterization tests stand a fake server on.
    #[cfg(test)]
    pub(crate) fn use_cloud(
        &mut self,
        repo: Arc<dyn ConversationRepository>,
        api: Arc<dyn HiveConversationsApi>,
        hive_url: &str,
    ) {
        self.cloud.repo = Some(repo);
        self.cloud.api = Some(api);
        self.cloud.hive_url = Some(hive_url.to_string());
    }

    /// The saved conversations, and the account's server when signed in.
    /// Whatever cannot be had is left unset; the commands say what is missing.
    pub(super) async fn cloud_connect(&mut self) {
        if self.cloud.repo.is_none() {
            match ConversationSqliteRepository::deferred() {
                Ok(repo) => self.cloud.repo = Some(Arc::new(repo)),
                Err(error) => tracing::warn!(%error, "cannot open the conversation store"),
            }
        }
        if self.cloud.api.is_none()
            && let Some(auth) = crate::hive_auth::signed_in_session().await
        {
            let hive = chatty_core::hive_settings_repository()
                .load()
                .await
                .unwrap_or_default();
            let url = hive.registry_url().to_string();
            self.cloud.api = Some(Arc::new(HttpHiveConversationsApi::new(
                &url,
                Some(auth.clone()),
            )));
            self.cloud.auth = Some(auth);
            self.cloud.hive_url = Some(url);
        }
    }

    /// A sign-in or sign-out happened: what was learned under the old one
    /// (the account's server and sign-in, this device's id, the sync cursor)
    /// is dropped and learned again on next use.
    pub(crate) fn forget_cloud_sign_in(&mut self) {
        self.cloud.api = None;
        self.cloud.auth = None;
        self.cloud.hive_url = None;
        self.cloud.sync = None;
        self.cloud.this_device = None;
        super::forget_this_device();
        self.cloud.held_by.clear();
    }

    /// `/conversations`: pull the account's list, then show every saved
    /// conversation with where it runs and what its turn is doing.
    pub async fn conversations_command(&mut self) -> String {
        self.cloud_connect().await;
        match self.list_conversations().await {
            Ok(text) => text,
            Err(error) => format!("Could not list conversations: {error:#}"),
        }
    }

    async fn list_conversations(&mut self) -> Result<String> {
        let repo = self
            .cloud
            .repo
            .clone()
            .context("The conversation store is not available.")?;
        let mut note = None;
        match (self.cloud.api.clone(), self.cloud.hive_url.clone()) {
            (Some(api), Some(url)) => {
                let local = repo.load_metadata().await?;
                let engine = self.cloud.sync.get_or_insert_with(|| SyncEngine::new(&url));
                match pull(api.as_ref(), engine, &local).await {
                    Ok(actions) => {
                        let open = self.session.conversation().map(|c| c.id().to_string());
                        let actions: Vec<SyncAction> = actions
                            .into_iter()
                            .filter(|action| {
                                !matches!(action, SyncAction::ApplyTombstone { local_id }
                                    if open.as_deref() == Some(local_id.as_str()))
                            })
                            .collect();
                        for action in &actions {
                            if let SyncAction::UpsertMetadata(row) = action {
                                self.cloud
                                    .status
                                    .insert(row.local_id.clone(), row.turn_status);
                                match &row.device_name {
                                    Some(name) => {
                                        self.cloud
                                            .held_by
                                            .insert(row.local_id.clone(), name.clone());
                                    }
                                    None => {
                                        self.cloud.held_by.remove(&row.local_id);
                                    }
                                }
                            }
                        }
                        apply_to_repository(repo.as_ref(), &actions).await?;
                        // Deleted on the account while a device held it
                        // (AGE-2121): the copy stays here, local-only.
                        let detached = actions
                            .iter()
                            .filter(|action| matches!(action, SyncAction::DetachDeleted { .. }))
                            .count();
                        if detached > 0 {
                            note = Some(format!(
                                "{detached} conversation(s) were deleted on your Hive account. They stay on this device."
                            ));
                        }
                    }
                    Err(error) => note = Some(format!("Showing saved conversations only: {error}")),
                }
            }
            _ => note = Some(chatty_core::tui_hive::RUN_LOGIN.to_string()),
        }
        let metadata = repo.load_metadata().await?;
        if self.cloud.this_device.is_none()
            && metadata.iter().any(|row| {
                matches!(
                    row.sync.synced().map(|synced| &synced.placement),
                    Some(chatty_core::models::conversation::Placement::Device { .. })
                )
            })
            && let Some(url) = self.cloud.hive_url.clone()
        {
            let server = chatty_core::session::HandoffServer::new(&url, self.cloud.auth.clone());
            self.cloud.this_device = chatty_core::session::this_device_id(&server).await.ok();
        }
        let entries = conversations::entries(
            &metadata,
            &self.cloud.status,
            &self.cloud.held_by,
            self.cloud.this_device.as_deref(),
        );
        self.cloud.listing = entries.iter().map(|entry| entry.id.clone()).collect();
        let open = self.session.conversation().map(|c| c.id().to_string());
        let mut text = conversations::render(&entries, open.as_deref());
        if let Some(note) = note {
            text.push_str(&format!("\n{note}"));
        }
        Ok(text)
    }

    /// `/open <n|id>`: make a saved conversation the one on screen. A synced
    /// conversation whose copy is stale is read from the server first; one
    /// that runs in the cloud also loads what its run waits on and follows
    /// the turn if one is running.
    pub async fn open_command(&mut self, arg: &str) {
        if let Err(error) = self.open_conversation(arg).await {
            self.add_system_message(format!("{error:#}"));
        }
    }

    async fn open_conversation(&mut self, arg: &str) -> Result<()> {
        if self.is_streaming {
            return Err(anyhow!(
                "Wait for the running turn to finish, or stop it first."
            ));
        }
        self.cloud_connect().await;
        let repo = self
            .cloud
            .repo
            .clone()
            .context("The conversation store is not available.")?;
        if self.cloud.listing.is_empty() {
            let metadata = repo.load_metadata().await?;
            self.cloud.listing = metadata.into_iter().map(|row| row.id).collect();
        }
        let id = conversations::resolve(&self.cloud.listing, arg).map_err(|e| anyhow!(e))?;
        let data = repo
            .load_one(&id)
            .await?
            .with_context(|| format!("Conversation {id} is not saved on this device."))?;

        let mut ctx = self.build_agent_context();
        ctx.mcp_tools = match self.mcp_service {
            Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
            None => None,
        };
        self.session
            .restore_conversation(data, &self.model_config, &self.provider_config, ctx)
            .await
            .context("Failed to open the conversation")?;
        // A background init that has not finished must not replace it.
        self.init_generation += 1;
        self.cloud.following = false;
        self.cloud.attach_failed = false;
        self.cloud.accept_gap = false;
        self.cloud.detail_in_flight = false;
        self.cloud.nonces.clear();

        let mut notice = None;
        let sync = self
            .session
            .conversation()
            .map(|conv| conv.sync().clone())
            .unwrap_or_default();
        self.hosted = match sync.cloud_on() {
            Some((url, remote_id)) => {
                let auth = crate::hive_auth::signed_in_session().await;
                Some(self.new_hosted(url, remote_id, auth))
            }
            None => None,
        };

        if let Some(synced) = sync.synced() {
            // Stale: never read, or invalidated by a newer version. Unknown
            // status: nothing here has said whether a turn is running.
            let stale = synced.last_synced_at.is_none();
            let unknown_status = sync.runs_in_cloud() && !self.cloud.status.contains_key(&id);
            if stale || unknown_status {
                notice = self.fetch_and_adopt(&synced.remote_id.clone()).await;
            }
        }
        self.show_conversation();
        if let Some(notice) = notice {
            self.add_system_message(notice);
        }
        if self.hosted.is_some() {
            self.load_cloud_inputs().await;
            self.attach_if_running();
        }
        Ok(())
    }

    /// A hosted client whose non-event frames come back as events.
    pub(super) fn new_hosted(
        &self,
        url: &str,
        remote_id: &str,
        auth: Option<Arc<chatty_core::hive::HiveSession>>,
    ) -> HostedSession {
        let tx = self.event_tx.clone();
        let mut hosted = HostedSession::new(url, remote_id).with_auth(auth);
        hosted.set_frame_sink(Arc::new(move |frame| {
            let _ = tx.send(AppEvent::CloudFrame(frame));
        }));
        hosted
    }

    /// Read the open conversation from the server and adopt it; the notice
    /// to show when that could not be done.
    async fn fetch_and_adopt(&mut self, remote_id: &str) -> Option<String> {
        let Some(api) = self.cloud.api.clone() else {
            return Some(chatty_core::tui_hive::RUN_LOGIN.to_string());
        };
        match api.conversation(remote_id).await {
            Ok(detail) => match self.adopt(detail).await {
                Ok(()) => None,
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "could not adopt a cloud conversation"
                    );
                    Some(COULD_NOT_UPDATE.to_string())
                }
            },
            Err(error) => {
                tracing::warn!(%error, "could not fetch a cloud conversation");
                Some(notice_of(&error))
            }
        }
    }

    /// Replace the open conversation's history with the server's and save it.
    async fn adopt(&mut self, detail: ConversationDetail) -> Result<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0);
        let conversation = self
            .session
            .conversation_mut()
            .context("no conversation is open")?;
        let status = adopt_detail(conversation, detail, now_ms)?;
        let id = conversation.id().to_string();
        self.cloud.status.insert(id, status);
        if let (Some(repo), Some(conversation)) =
            (self.cloud.repo.clone(), self.session.conversation())
        {
            let data = conversation.to_conversation_data()?;
            repo.save(conversation.id(), data).await?;
        }
        Ok(())
    }

    /// Put the open conversation on screen: its history as transcript rows,
    /// its title, fresh counters.
    pub(super) fn show_conversation(&mut self) {
        let Some(conversation) = self.session.conversation() else {
            return;
        };
        let messages = conversation.messages();
        let title = conversation.title().to_string();
        self.transcript.clear();
        for message in &messages {
            self.append_missed_message(message);
        }
        self.title = title;
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cache_read_tokens = 0;
        self.total_cache_write_tokens = 0;
        self.last_turn_usage = None;
        self.swarm_trace = chatty_core::services::swarm_trace::SwarmTrace::new();
        self.pending_approval = None;
        self.pending_clarification = None;
        self.is_ready = true;
        self.pin_to_bottom();
    }

    /// Load the approvals and questions the conversation's run waits on.
    async fn load_cloud_inputs(&mut self) {
        let Some(fetch) = self.hosted.as_ref().map(HostedSession::fetch_inputs) else {
            return;
        };
        match fetch.await {
            Ok(inputs) => {
                for input in inputs {
                    self.handle_cloud_frame(HostedFrame::BrokerInput {
                        id: input.id,
                        nonce: input.nonce,
                        request: input.request,
                    });
                }
            }
            Err(error) => tracing::debug!(%error, "could not load the inputs a cloud run waits on"),
        }
    }

    fn attach_if_running(&mut self) {
        let id = self.session.conversation().map(|c| c.id().to_string());
        let busy = id
            .and_then(|id| self.cloud.status.get(&id).copied())
            .is_some_and(|status| conversations::mark_of(status).is_some());
        if busy && !self.cloud.attach_failed {
            let accept_gap = std::mem::take(&mut self.cloud.accept_gap);
            self.attach_cloud(accept_gap);
        }
    }

    /// Follow the turn running on the server, whoever started it.
    fn attach_cloud(&mut self, accept_gap: bool) {
        if self.is_streaming {
            return;
        }
        let tx = self.event_tx.clone();
        let Some(hosted) = self.hosted.as_mut() else {
            return;
        };
        let events = tx.clone();
        let turn = hosted.attach(Arc::new(AtomicBool::new(false)), accept_gap, move |event| {
            // The loop's own continuation is the server's business here.
            if !matches!(event, chatty_core::session::SessionEvent::FollowUp(_)) {
                let _ = events.send(AppEvent::from(event));
            }
        });
        match turn {
            Ok(turn) => {
                self.transcript.start_assistant();
                self.is_streaming = true;
                self.cloud.following = true;
                tokio::spawn(async move {
                    let outcome = turn.await;
                    let _ = tx.send(AppEvent::CloudAttachEnded(outcome));
                });
            }
            Err(error) => tracing::warn!(%error, "could not follow the cloud turn"),
        }
    }

    /// Whether a followed cloud turn is on screen: the server owns it, so
    /// the turn is not finalized here.
    pub(super) fn is_following_cloud(&self) -> bool {
        self.cloud.following
    }

    /// A followed turn is over: record what became of it, then read the
    /// server's copy, which holds the reply.
    pub(super) fn finish_cloud_attach(&mut self, outcome: AttachOutcome) {
        self.cloud.following = false;
        self.transcript.finish_streaming();
        self.reset_stream_state();
        if matches!(outcome, AttachOutcome::Lost | AttachOutcome::Busy) {
            self.cloud.attach_failed = true;
        }
        if outcome == AttachOutcome::Lost {
            self.add_system_message(
                "Lost the connection to the cloud run. Open the conversation again to follow it."
                    .to_string(),
            );
        }
        if outcome == AttachOutcome::NeedsDetail {
            self.cloud.accept_gap = true;
        } else if let Some(id) = self.session.conversation().map(|c| c.id().to_string()) {
            self.cloud.status.insert(id, TurnStatus::Idle);
        }
        self.spawn_detail_fetch();
    }

    fn spawn_detail_fetch(&mut self) {
        let remote_id = self
            .session
            .conversation()
            .and_then(|c| c.sync().synced().map(|s| s.remote_id.clone()));
        let (Some(api), Some(remote_id)) = (self.cloud.api.clone(), remote_id) else {
            return;
        };
        if std::mem::replace(&mut self.cloud.detail_in_flight, true) {
            return;
        }
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let result = api
                .conversation(&remote_id)
                .await
                .map_err(|error| notice_of(&error));
            let _ = tx.send(AppEvent::CloudDetail(Box::new(result)));
        });
    }

    /// The server's copy arrived (or could not be had).
    pub(super) fn finish_cloud_detail(&mut self, result: Result<ConversationDetail, String>) {
        self.cloud.detail_in_flight = false;
        let detail = match result {
            Ok(detail) => detail,
            Err(notice) => {
                self.add_system_message(notice);
                return;
            }
        };
        // A message sent while the turn was followed starts the next turn
        // when this one ends; replacing the history under it would drop it.
        if self.is_streaming {
            return;
        }
        let event_tx = self.event_tx.clone();
        let Some(conversation) = self.session.conversation_mut() else {
            return;
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0);
        match adopt_detail(conversation, detail, now_ms) {
            Ok(status) => {
                let id = conversation.id().to_string();
                self.cloud.status.insert(id, status);
                self.persist_synced();
                self.show_conversation();
                self.attach_if_running();
            }
            Err(error) => {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "could not adopt the server's history"
                );
                let _ = event_tx.send(AppEvent::Notice(COULD_NOT_UPDATE.to_string()));
            }
        }
    }

    /// Save the open conversation when it is synced: the terminal's own
    /// conversations are not kept, a synced one is (HX-19).
    pub(super) fn persist_synced(&mut self) {
        let Some(conversation) = self.session.conversation() else {
            return;
        };
        if conversation.sync().synced().is_none() {
            return;
        }
        if self.cloud.repo.is_none() {
            self.cloud.repo = ConversationSqliteRepository::deferred()
                .ok()
                .map(|repo| Arc::new(repo) as Arc<dyn ConversationRepository>);
        }
        let Some(repo) = self.cloud.repo.clone() else {
            return;
        };
        match conversation.to_conversation_data() {
            Ok(data) => {
                let save = repo.save(conversation.id(), data);
                tokio::spawn(async move {
                    if let Err(error) = save.await {
                        tracing::warn!(%error, "could not save the synced conversation");
                    }
                });
            }
            Err(error) => tracing::warn!(%error, "could not serialize the synced conversation"),
        }
    }

    // ── Inputs a cloud run waits on (HX-11) ──────────────────────────────

    /// A frame from the server that is not a session event.
    pub(super) fn handle_cloud_frame(&mut self, frame: HostedFrame) {
        match frame {
            HostedFrame::BrokerInput { id, nonce, request } => {
                if self.cloud.settled.contains(&id)
                    || self.pending_approval.as_ref().is_some_and(|a| a.id == id)
                    || self
                        .pending_clarification
                        .as_ref()
                        .is_some_and(|c| c.id == id)
                {
                    return;
                }
                self.cloud.nonces.insert(id.clone(), nonce);
                match request {
                    BrokerRequest::Approval(request) => {
                        let detail = ApprovalDetail::forwarded(request);
                        let command = detail.description();
                        self.transcript.approval_requested(
                            id.clone(),
                            command.clone(),
                            false,
                            detail.asker.clone(),
                        );
                        self.pending_approval = Some(PendingApproval {
                            id,
                            command,
                            is_sandboxed: false,
                            asker: detail.asker,
                            shown_at: std::time::Instant::now(),
                        });
                    }
                    BrokerRequest::Question(request) => {
                        self.pending_clarification = Some(PendingClarification {
                            id,
                            questions: ClarifyingQuestion::forwarded(request),
                            current: 0,
                            answers: Vec::new(),
                            custom: None,
                        });
                    }
                }
            }
            HostedFrame::BrokerInputSettled { id, outcome } => {
                self.settle_cloud_input(&id, outcome == InputOutcome::Approved, None);
            }
            HostedFrame::InputStale { id } => {
                self.settle_cloud_input(&id, false, Some(ALREADY_ANSWERED));
            }
            HostedFrame::ConnectorNeedsSignIn { connector } => {
                let mut text = format!("The cloud run needs you to sign in to {connector}.");
                if let Some(url) = self
                    .cloud
                    .hive_url
                    .as_deref()
                    .and_then(connector_settings_url)
                {
                    text.push_str(&format!(" Do it at {url}"));
                }
                self.add_system_message(text);
            }
            HostedFrame::Session(_)
            | HostedFrame::Attached { .. }
            | HostedFrame::StreamError(_)
            | HostedFrame::Unknown(_) => {}
        }
    }

    /// An input is done with: its prompt goes, and a replay of its frame
    /// must not bring it back.
    fn settle_cloud_input(&mut self, id: &str, approved: bool, notice: Option<&str>) {
        self.cloud.nonces.remove(id);
        self.cloud.settled.insert(id.to_string());
        if self.pending_approval.as_ref().is_some_and(|a| a.id == id) {
            self.pending_approval = None;
            self.transcript.approval_resolved(id, approved);
        }
        if self
            .pending_clarification
            .as_ref()
            .is_some_and(|c| c.id == id)
        {
            self.pending_clarification = None;
        }
        if let Some(notice) = notice {
            self.add_system_message(notice.to_string());
        }
    }

    /// The answer to a cloud run's input, when `id` is one: it goes to the
    /// input's own route with the nonce it came with, not to a store here.
    pub(super) fn answer_cloud_input(&mut self, id: &str, answer: InputAnswer) -> Option<Answer> {
        let nonce = self.cloud.nonces.remove(id)?;
        let send = self.hosted.as_ref()?.answer_input(id, &nonce, answer);
        let tx = self.event_tx.clone();
        let id = id.to_string();
        self.cloud.settled.insert(id.clone());
        Some(Box::pin(async move {
            match send.await {
                AnswerOutcome::Accepted => {}
                AnswerOutcome::Stale => {
                    let _ = tx.send(AppEvent::CloudFrame(HostedFrame::InputStale { id }));
                }
                AnswerOutcome::Failed(error) => {
                    tracing::warn!(%error, "failed to answer a cloud run's input");
                }
            }
        }))
    }

    /// An approval answer: a cloud input's route, else `None`.
    pub(super) fn answer_cloud_approval(&mut self, id: &str, approved: bool) -> Option<Answer> {
        self.answer_cloud_input(id, InputAnswer::Approval(approved))
    }

    /// A question's answers: a cloud input's route, else `None`.
    pub(super) fn answer_cloud_questions(
        &mut self,
        id: &str,
        answers: Vec<ClarificationAnswer>,
    ) -> Option<Answer> {
        self.answer_cloud_input(id, InputAnswer::Answers(answers))
    }
}

/// What to tell the user about a failed read of a cloud conversation.
fn notice_of(error: &ApiError) -> String {
    match error {
        ApiError::SignedOut => error.to_string(),
        ApiError::NotFound => NO_LONGER_EXISTS.to_string(),
        _ => COULD_NOT_UPDATE.to_string(),
    }
}
