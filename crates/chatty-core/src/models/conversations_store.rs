use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use hive_client::HiveSession;

use crate::models::clarification_store::ClarificationAnswer;
use crate::models::execution_approval_store::ApprovalDecision;
use crate::models::write_approval_store::WriteApprovalDecision;
use crate::repositories::ConversationMetadata;
use crate::services::hive_sync::TurnStatus;
use crate::session::{
    AgentSession, AnswerOutcome, HostedFrame, HostedSession, InputAnswer, hosted::FrameSink,
};

use super::conversation::{Conversation, ConversationSync};

/// Maximum number of full conversation objects kept in memory.
/// When the cache exceeds this limit, the least recently used conversations are evicted.
/// They will be re-loaded from SQLite on demand if the user navigates back.
const MAX_CACHED_CONVERSATIONS: usize = 10;

/// What the sidebar shows beside a conversation that runs in the cloud
/// (HX-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudMark {
    /// A turn is running on the server.
    Running,
    /// A turn is waiting for an answer from the user.
    WaitingForYou,
    /// The server restarted mid-turn; the turn can be resumed (HX-22).
    Interrupted,
    /// A turn finished since this conversation was last opened.
    Done,
    /// A turn failed since this conversation was last opened.
    Failed,
}

impl CloudMark {
    /// The words both frontends show for the mark.
    pub fn label(self) -> &'static str {
        match self {
            CloudMark::Running => "Running in the cloud",
            CloudMark::WaitingForYou => "Waiting for you",
            CloudMark::Interrupted => "Interrupted",
            CloudMark::Done => "Done",
            CloudMark::Failed => "Something went wrong",
        }
    }
}

/// A frame from a hosted conversation that is not a session event, with the
/// local conversation it came from.
pub type HostedUiFrame = (String, HostedFrame);

/// An approval or question a cloud run is waiting on: the address the
/// answer needs (HX-11).
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingInputRef {
    conversation_id: String,
    nonce: String,
}

/// Global store for all conversations.
///
/// Two-layer design:
/// - `metadata`: always loaded at startup (lightweight — just id/title/cost)
/// - `sessions`: lazily populated when a conversation is selected, with LRU eviction
///
/// Each loaded conversation lives inside its own [`AgentSession`] (AGE-195):
/// the session owns the conversation, the per-agent approval stores its
/// tools raise requests on, and the turn in flight, so two conversations
/// streaming at once never share a channel.
pub struct ConversationsStore {
    /// Lightweight metadata list, sorted by updated_at descending.
    /// This is the source of truth for the sidebar and navigation.
    metadata: Vec<ConversationMetadata>,
    /// Full conversation data, populated on demand when the user selects a conversation.
    /// Bounded to `MAX_CACHED_CONVERSATIONS` entries via LRU eviction.
    sessions: HashMap<String, AgentSession>,
    /// The hosted client for each loaded conversation whose turns run in the
    /// cloud (AGE-298, AGE-2088). Keyed like `sessions` and derived from the
    /// conversation's own sync state when it is loaded, so the two cannot
    /// disagree about where a conversation runs. A conversation always has a session;
    /// it has an entry here as well only while it runs elsewhere.
    hosted: HashMap<String, HostedSession>,
    /// The Hive sign-in every hosted client authenticates with (AGE-835): a
    /// `chatty-server` validates the same login session the registry issued.
    /// `None` until the frontend installs its session.
    hosted_auth: Option<Arc<HiveSession>>,
    /// This device's login session id on the Hive server (HX-14), fetched
    /// once per sign-in and dropped when the sign-in changes.
    this_device: Option<String>,
    /// Where a hosted conversation's non-event frames go: the owner reads the
    /// other end (HX-11). Installed once; every hosted client's sink writes
    /// to it, tagged with its conversation.
    frame_tx: Option<futures::channel::mpsc::UnboundedSender<HostedUiFrame>>,
    /// Whether each synced conversation's turn is running on the server, from
    /// the last pull. Live state: never persisted (HX-10).
    turn_status: HashMap<String, TurnStatus>,
    /// The name of the device holding each conversation, from the last pull.
    device_names: HashMap<String, String>,
    /// Conversations whose turn finished / failed since they were last
    /// opened, for the sidebar's mark.
    finished: HashSet<String>,
    failed: HashSet<String>,
    /// Approvals and questions cloud runs wait on, by input id.
    /// Behind a lock because the answer routes (`resolve_*_remotely`) only
    /// borrow the store shared.
    inputs: std::sync::Mutex<HashMap<String, PendingInputRef>>,
    /// Tracks access order for LRU eviction. Most recently used at the back.
    access_order: VecDeque<String>,
    active_conversation_id: Option<String>,
    /// IDs of conversations that have an active LLM stream. These are protected from eviction
    /// to avoid losing in-flight streaming state.
    streaming_ids: HashSet<String>,
}

impl ConversationsStore {
    pub fn new() -> Self {
        Self {
            metadata: Vec::new(),
            sessions: HashMap::new(),
            hosted: HashMap::new(),
            hosted_auth: None,
            this_device: None,
            frame_tx: None,
            turn_status: HashMap::new(),
            device_names: HashMap::new(),
            finished: HashSet::new(),
            failed: HashSet::new(),
            inputs: std::sync::Mutex::new(HashMap::new()),
            access_order: VecDeque::new(),
            active_conversation_id: None,
            streaming_ids: HashSet::new(),
        }
    }

    // ── Metadata layer ────────────────────────────────────────────────────────

    /// Replace the metadata list (called once at startup after `load_metadata()`).
    pub fn set_metadata(&mut self, metadata: Vec<ConversationMetadata>) {
        self.metadata = metadata;
    }

    /// Total number of conversations (based on metadata, not the in-memory cache).
    pub fn count(&self) -> usize {
        self.metadata.len()
    }

    /// Return up to `limit` conversations as sidebar tuples (id, title, cost).
    pub fn list_recent_metadata(&self, limit: usize) -> Vec<(String, String, Option<f64>)> {
        self.metadata
            .iter()
            .take(limit)
            .map(|m| (m.id.clone(), m.title.clone(), Some(m.total_cost)))
            .collect()
    }

    /// All conversation IDs sorted by updated_at descending (for keyboard navigation).
    pub fn all_metadata_ids(&self) -> Vec<String> {
        self.metadata.iter().map(|m| m.id.clone()).collect()
    }

    /// Do this conversation's turns run on a server (AGE-298)?
    ///
    /// Answered from the metadata layer, so the sidebar can badge a cloud
    /// conversation without loading it — which is the point of that layer.
    /// A loaded conversation is asked directly, because it is the fresher of
    /// the two right after a move.
    pub fn is_hosted(&self, id: &str) -> bool {
        if let Some(conversation) = self.get_conversation(id) {
            return conversation.sync().runs_in_cloud();
        }
        self.metadata
            .iter()
            .find(|m| m.id == id)
            .is_some_and(|m| m.sync.runs_in_cloud())
    }

    /// Every conversation's metadata row, newest first: the local side of a
    /// sync pull (AGE-2088).
    pub fn metadata(&self) -> &[ConversationMetadata] {
        &self.metadata
    }

    /// Insert or update a single metadata entry and re-sort by updated_at descending.
    pub fn upsert_metadata(&mut self, id: &str, title: &str, total_cost: f64, updated_at: i64) {
        // The sync state is not a parameter: every caller of this is
        // reporting a title/cost change after a turn, and none of them knows
        // where the conversation runs. It is set by a move or a pull, through
        // [`set_conversation_sync`](Self::set_conversation_sync), and read
        // back from the row on the next load. The turn totals (AGE-351) are
        // taken from the loaded conversation the same way: the session
        // accumulates them at the turn barrier, and the row carries them.
        let loaded = self.get_conversation(id).map(|conversation| {
            (
                conversation.sync().clone(),
                conversation.tool_call_count(),
                conversation.context_tokens(),
            )
        });
        if let Some(entry) = self.metadata.iter_mut().find(|m| m.id == id) {
            entry.title = title.to_string();
            entry.total_cost = total_cost;
            entry.updated_at = updated_at;
            if let Some((sync, tool_call_count, context_tokens)) = loaded {
                entry.sync = sync;
                entry.tool_call_count = tool_call_count;
                entry.context_tokens = context_tokens;
            }
        } else {
            let (sync, tool_call_count, context_tokens) = loaded.unwrap_or_default();
            self.metadata.push(ConversationMetadata {
                id: id.to_string(),
                title: title.to_string(),
                total_cost,
                updated_at,
                sync,
                tool_call_count,
                context_tokens,
            });
        }
        // Keep sorted: most recently updated first
        self.metadata
            .sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    }

    /// Remove a conversation from the metadata list.
    pub fn remove_metadata(&mut self, id: &str) {
        self.metadata.retain(|m| m.id != id);
    }

    // ── Full conversation cache ───────────────────────────────────────────────

    /// Returns true if the full conversation data is already in memory.
    pub fn is_loaded(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    /// Insert a lazily-loaded conversation, wrapped in its session, into the
    /// cache. A session with no conversation is not a loaded conversation
    /// and is dropped. Evicts the least recently used non-active,
    /// non-streaming conversations if the cache exceeds
    /// `MAX_CACHED_CONVERSATIONS`.
    pub fn insert_loaded(&mut self, session: AgentSession) {
        let Some(id) = session.conversation().map(|c| c.id().to_string()) else {
            tracing::warn!("Ignoring a session with no conversation");
            return;
        };
        // The hosted client is derived from the conversation's own sync
        // state, here and nowhere else, so "where the row says it runs" and
        // "where turns actually go" cannot drift apart (AGE-298).
        let sync = session.conversation().map(|c| c.sync().clone());
        self.follow_placement(&id, sync.as_ref());
        self.sessions.insert(id.clone(), session);
        self.touch_access_order(&id);
        self.evict_if_needed();
    }

    /// Authenticate every hosted client, present and future, as the user
    /// signed in to `auth`.
    pub fn set_hosted_auth(&mut self, auth: Option<Arc<HiveSession>>) {
        for client in self.hosted.values_mut() {
            client.set_auth(auth.clone());
        }
        self.hosted_auth = auth;
        self.this_device = None;
    }

    /// This device's login session id, when fetched for the current sign-in.
    pub fn this_device(&self) -> Option<String> {
        self.this_device.clone()
    }

    /// Remember this device's login session id for the current sign-in, or
    /// forget it (`None`) when the sign-in ends.
    pub fn set_this_device(&mut self, device_id: Option<String>) {
        self.this_device = device_id;
    }

    /// The Hive sign-in hosted requests authenticate with, for a move that
    /// talks to a server before any hosted client exists.
    pub fn hosted_auth(&self) -> Option<Arc<HiveSession>> {
        self.hosted_auth.clone()
    }

    /// The hosted client for a conversation, when its turns run on a server.
    pub fn get_hosted(&self, id: &str) -> Option<&HostedSession> {
        self.hosted.get(id)
    }

    /// The two things starting a turn needs: the conversation's session, and
    /// its hosted client when it has one.
    ///
    /// Handed out together because
    /// [`turn_transport::begin_turn`](crate::session::transport::begin_turn)
    /// needs both mutably and they live in different maps — a caller taking
    /// them one at a time would be borrowing the store twice.
    pub fn turn_targets(
        &mut self,
        id: &str,
    ) -> Option<(&mut AgentSession, Option<&mut HostedSession>)> {
        let session = self.sessions.get_mut(id)?;
        Some((session, self.hosted.get_mut(id)))
    }

    /// Record a conversation's new sync state after a move or a pull
    /// completed.
    ///
    /// Called *after* the history transfer succeeded — never before, so a
    /// failed move leaves the row, the metadata and the hosted client all
    /// describing the place the conversation is still running.
    pub fn set_conversation_sync(&mut self, id: &str, sync: ConversationSync) {
        self.follow_placement(id, Some(&sync));
        if let Some(entry) = self.metadata.iter_mut().find(|m| m.id == id) {
            entry.sync = sync.clone();
        }
        if let Some(conversation) = self.get_conversation_mut(id) {
            conversation.set_sync(sync);
        }
    }

    /// Apply a sync pull's actions to the in-memory layer (AGE-2088): the
    /// sidebar's metadata, every loaded conversation's title and sync state,
    /// and the hosted clients that follow the placement.
    ///
    /// Returns the actions to carry out on the persisted rows: every one,
    /// less the tombstones of a conversation that is open or streaming,
    /// which are left for a later pull rather than pulled out from under the
    /// user (a full pull at the next start deletes them).
    pub fn apply_sync_actions(
        &mut self,
        actions: &[crate::services::hive_sync::SyncAction],
    ) -> Vec<crate::services::hive_sync::SyncAction> {
        use crate::services::hive_sync::{SyncAction, apply_to_rows};

        let kept: Vec<SyncAction> = actions
            .iter()
            .filter(|action| match action {
                SyncAction::ApplyTombstone { local_id } => {
                    let busy = self.active_conversation_id.as_deref() == Some(local_id.as_str())
                        || self.streaming_ids.contains(local_id);
                    if busy {
                        tracing::info!(
                            conv_id = %local_id,
                            "Not deleting an open conversation the server deleted; a later pull will"
                        );
                    }
                    !busy
                }
                _ => true,
            })
            .cloned()
            .collect();

        apply_to_rows(&mut self.metadata, &kept);
        for action in &kept {
            match action {
                SyncAction::UpsertMetadata(remote) => {
                    self.set_turn_status(&remote.local_id, remote.turn_status);
                    match &remote.device_name {
                        Some(name) => {
                            self.device_names
                                .insert(remote.local_id.clone(), name.clone());
                        }
                        None => {
                            self.device_names.remove(&remote.local_id);
                        }
                    }
                    self.follow_placement(&remote.local_id, Some(&remote.sync));
                    if let Some(conversation) = self.get_conversation_mut(&remote.local_id) {
                        conversation.set_title(remote.title.clone());
                        conversation.set_sync(remote.sync.clone());
                    }
                }
                SyncAction::MarkDetailStale { local_id } => {
                    if let Some(conversation) = self.get_conversation_mut(local_id)
                        && let ConversationSync::Synced(synced) = conversation.sync()
                    {
                        let mut synced = synced.clone();
                        synced.last_synced_at = None;
                        conversation.set_sync(ConversationSync::Synced(synced));
                    }
                }
                SyncAction::ApplyTombstone { local_id } => {
                    self.sessions.remove(local_id);
                    self.hosted.remove(local_id);
                    self.access_order.retain(|id| id != local_id);
                }
                SyncAction::DetachDeleted { local_id } => {
                    self.device_names.remove(local_id);
                    self.follow_placement(local_id, None);
                    if let Some(conversation) = self.get_conversation_mut(local_id) {
                        conversation.set_sync(ConversationSync::LocalOnly);
                    }
                }
                SyncAction::FullResync => {}
            }
        }
        kept
    }

    /// Keep the hosted client in step with where `id`'s turns run: present
    /// exactly while its placement is the cloud.
    fn follow_placement(&mut self, id: &str, sync: Option<&ConversationSync>) {
        match sync.and_then(ConversationSync::cloud_on) {
            Some((server_url, remote_id)) => {
                let unchanged = self.hosted.get(id).is_some_and(|client| {
                    client.server_url() == server_url && client.remote_id() == remote_id
                });
                if !unchanged {
                    let mut client = HostedSession::new(server_url, remote_id)
                        .with_auth(self.hosted_auth.clone());
                    client.set_files_dir(crate::services::hive_files::cache_dir(remote_id));
                    if let Some(sink) = self.frame_sink(id) {
                        client.set_frame_sink(sink);
                    }
                    self.hosted.insert(id.to_string(), client);
                }
            }
            None => {
                self.hosted.remove(id);
            }
        }
    }

    /// Where the frames hosted conversations send that are not session events
    /// (broker inputs, connector sign-in, …) go. Present and future hosted
    /// clients write to `tx`, each frame tagged with its conversation.
    pub fn set_frame_channel(
        &mut self,
        tx: futures::channel::mpsc::UnboundedSender<HostedUiFrame>,
    ) {
        self.frame_tx = Some(tx);
        let ids: Vec<String> = self.hosted.keys().cloned().collect();
        for id in ids {
            if let Some(sink) = self.frame_sink(&id)
                && let Some(client) = self.hosted.get_mut(&id)
            {
                client.set_frame_sink(sink);
            }
        }
    }

    fn frame_sink(&self, conversation_id: &str) -> Option<FrameSink> {
        let tx = self.frame_tx.clone()?;
        let conversation_id = conversation_id.to_string();
        Some(Arc::new(move |frame| {
            // The receiver is the app; gone means the app is closing.
            let _ = tx.unbounded_send((conversation_id.clone(), frame));
        }))
    }

    /// The hosted client of a conversation, to attach to its running turn.
    pub fn hosted_mut(&mut self, id: &str) -> Option<&mut HostedSession> {
        self.hosted.get_mut(id)
    }

    // ── Cloud status (HX-10) ──────────────────────────────────────────────────

    /// Record what a pull saw of a conversation's turn on the server.
    ///
    /// A turn that stopped since the last pull leaves the conversation marked
    /// done until it is opened.
    pub fn set_turn_status(&mut self, id: &str, status: TurnStatus) {
        let before = self.turn_status.insert(id.to_string(), status);
        let was_busy = matches!(
            before,
            Some(TurnStatus::Running | TurnStatus::WaitingForYou)
        );
        let busy = matches!(status, TurnStatus::Running | TurnStatus::WaitingForYou);
        if was_busy && !busy && self.active_conversation_id.as_deref() != Some(id) {
            self.finished.insert(id.to_string());
        }
        if busy {
            self.finished.remove(id);
            self.failed.remove(id);
        }
    }

    /// The name of the device holding the conversation, as of the last pull.
    pub fn device_name(&self, id: &str) -> Option<&str> {
        self.device_names.get(id).map(String::as_str)
    }

    /// Whether the conversation's turn runs on the server right now, as of
    /// the last pull.
    pub fn turn_status(&self, id: &str) -> TurnStatus {
        self.turn_status.get(id).copied().unwrap_or_default()
    }

    /// A turn of this cloud conversation failed; shown until it is opened.
    pub fn mark_cloud_failed(&mut self, id: &str) {
        if self.active_conversation_id.as_deref() != Some(id) {
            self.failed.insert(id.to_string());
        }
    }

    /// The sidebar's mark for a conversation, `None` when there is nothing to
    /// say (not a cloud conversation, or idle with nothing new).
    pub fn cloud_mark(&self, id: &str) -> Option<CloudMark> {
        if !self.is_hosted(id) {
            return None;
        }
        match self.turn_status(id) {
            TurnStatus::Running => Some(CloudMark::Running),
            TurnStatus::WaitingForYou => Some(CloudMark::WaitingForYou),
            TurnStatus::Interrupted => Some(CloudMark::Interrupted),
            _ if self.failed.contains(id) => Some(CloudMark::Failed),
            _ if self.finished.contains(id) => Some(CloudMark::Done),
            _ => None,
        }
    }

    // ── Inputs a cloud run waits on (HX-11) ───────────────────────────────────

    /// Remember an input so an answer to its id can be addressed.
    pub fn remember_input(&self, conversation_id: &str, input_id: &str, nonce: &str) {
        if let Ok(mut inputs) = self.inputs.lock() {
            inputs.insert(
                input_id.to_string(),
                PendingInputRef {
                    conversation_id: conversation_id.to_string(),
                    nonce: nonce.to_string(),
                },
            );
        }
    }

    pub fn forget_input(&self, input_id: &str) {
        if let Ok(mut inputs) = self.inputs.lock() {
            inputs.remove(input_id);
        }
    }

    /// Answer a broker input if `input_id` is one; `None` when it is not (the
    /// caller then tries the conversation's own approvals). A refused-as-stale
    /// answer reports itself on the frame channel as
    /// [`HostedFrame::InputStale`], since the caller that spawns this future
    /// has nowhere to read an outcome.
    fn answer_broker_input(
        &self,
        input_id: &str,
        answer: InputAnswer,
    ) -> Option<futures::future::BoxFuture<'static, ()>> {
        let input = self.inputs.lock().ok()?.get(input_id)?.clone();
        let client = self.hosted.get(&input.conversation_id)?;
        let send = client.answer_input(input_id, &input.nonce, answer);
        let tx = self.frame_tx.clone();
        let conversation_id = input.conversation_id;
        let input_id = input_id.to_string();
        Some(Box::pin(async move {
            match send.await {
                AnswerOutcome::Accepted => {}
                AnswerOutcome::Stale => {
                    if let Some(tx) = tx {
                        let _ = tx.unbounded_send((
                            conversation_id,
                            HostedFrame::InputStale { id: input_id },
                        ));
                    }
                }
                AnswerOutcome::Failed(error) => {
                    tracing::warn!(%error, "failed to answer a cloud run's input");
                }
            }
        }))
    }

    /// Move an ID to the back of the access order (most recently used).
    fn touch_access_order(&mut self, id: &str) {
        self.access_order.retain(|s| s != id);
        self.access_order.push_back(id.to_string());
    }

    /// Evict least recently used non-protected conversations when cache exceeds the limit.
    /// Protected conversations: the active one and any with active streams.
    fn evict_if_needed(&mut self) {
        while self.sessions.len() > MAX_CACHED_CONVERSATIONS {
            let evict_id = self.find_lru_evictable();
            if let Some(id) = evict_id {
                self.sessions.remove(&id);
                // The hosted client is rebuilt from the conversation's sync state
                // when it is loaded again, so it is cache, not state.
                self.hosted.remove(&id);
                self.access_order.retain(|s| s != &id);
            } else {
                // All remaining conversations are protected — stop evicting
                break;
            }
        }
    }

    /// Find the least recently used conversation that is neither active nor streaming.
    fn find_lru_evictable(&self) -> Option<String> {
        // access_order front = oldest access, so iterate from front
        self.access_order
            .iter()
            .find(|id| {
                // Don't evict the active conversation
                self.active_conversation_id.as_deref() != Some(id.as_str())
                    // Don't evict conversations with active streams
                    && !self.streaming_ids.contains(id.as_str())
            })
            .cloned()
    }

    /// Mark a conversation as having an active stream (protects it from eviction).
    pub fn mark_streaming(&mut self, id: &str) {
        self.streaming_ids.insert(id.to_string());
    }

    /// Remove the streaming mark from a conversation (allows eviction again).
    /// Is a turn running on this conversation?
    ///
    /// The guard on a move: AGE-298 refuses one mid-turn, because a pending
    /// approval or clarification lives in the stores of the session that
    /// raised it and moving would leave it unanswerable.
    pub fn is_streaming(&self, id: &str) -> bool {
        self.streaming_ids.contains(id)
    }

    pub fn unmark_streaming(&mut self, id: &str) {
        self.streaming_ids.remove(id);
    }

    /// Get a conversation by ID (immutable). Returns `None` if not yet loaded.
    pub fn get_conversation(&self, id: &str) -> Option<&Conversation> {
        self.sessions.get(id).and_then(|s| s.conversation())
    }

    /// Get a mutable reference to a conversation by ID.
    pub fn get_conversation_mut(&mut self, id: &str) -> Option<&mut Conversation> {
        self.sessions.get_mut(id).and_then(|s| s.conversation_mut())
    }

    /// The session owning a loaded conversation.
    pub fn get_session(&self, id: &str) -> Option<&AgentSession> {
        self.sessions.get(id)
    }

    pub fn get_session_mut(&mut self, id: &str) -> Option<&mut AgentSession> {
        self.sessions.get_mut(id)
    }

    /// Resolve an execution approval by request id, whichever loaded
    /// conversation's agent raised it. Ids are unique, so at most one
    /// session has it; returns whether one did.
    pub fn resolve_execution_approval(&self, id: &str, decision: ApprovalDecision) -> bool {
        self.sessions
            .values()
            .any(|s| s.execution_approvals().resolve(id, decision.clone()))
    }

    /// Resolve an approval of either kind by request id: the execution
    /// store first (shell commands), then the write store (filesystem
    /// writes). The UI raises both through the same buttons.
    pub fn resolve_approval(&self, id: &str, approved: bool) -> bool {
        let decision = if approved {
            ApprovalDecision::Approved
        } else {
            ApprovalDecision::Denied
        };
        if self.resolve_execution_approval(id, decision) {
            return true;
        }
        let decision = if approved {
            WriteApprovalDecision::Approved
        } else {
            WriteApprovalDecision::Denied
        };
        self.resolve_write_approval(id, decision)
    }

    /// Resolve a filesystem write approval by request id; see
    /// [`resolve_execution_approval`](Self::resolve_execution_approval).
    pub fn resolve_write_approval(&self, id: &str, decision: WriteApprovalDecision) -> bool {
        self.sessions
            .values()
            .any(|s| s.write_approvals().resolve(id, decision.clone()))
    }

    /// Answer an `ask_user` clarification by request id; see
    /// [`resolve_execution_approval`](Self::resolve_execution_approval).
    pub fn resolve_clarification(&self, id: &str, answers: Vec<ClarificationAnswer>) -> bool {
        self.sessions
            .values()
            .any(|s| s.clarifications().resolve(id, answers.clone()))
    }

    /// Answer an approval raised by a *hosted* conversation's agent.
    ///
    /// A hosted request has no local store to resolve it in — it lives on the
    /// server, addressed by the same id — so the answer goes over the wire.
    /// The caller spawns the returned future; use it when the local
    /// [`resolve_approval`](Self::resolve_approval) found nothing, which is
    /// exactly the hosted case.
    ///
    /// It is sent to every hosted conversation with a turn running rather than
    /// to one guessed by id: the server answers `409` for a request it does
    /// not hold, so the wrong address is harmless and the right one wins. That
    /// keeps the same "ids are unique, ask everyone" rule the local path uses.
    pub fn resolve_approval_remotely(
        &self,
        id: &str,
        approved: bool,
    ) -> impl Future<Output = ()> + use<> {
        // An approval a worker in the cloud run raised is answered on the
        // input's own route, with its nonce (HX-11).
        let broker = self.answer_broker_input(id, InputAnswer::Approval(approved));
        let sends: Vec<_> = self
            .hosted
            .values()
            .filter(|remote| remote.is_turn_active())
            .map(|remote| remote.resolve_approval(id, approved))
            .collect();
        async move {
            match broker {
                Some(broker) => broker.await,
                None => {
                    futures::future::join_all(sends).await;
                }
            }
        }
    }

    /// Answer an `ask_user` raised by a hosted conversation's agent; see
    /// [`resolve_approval_remotely`](Self::resolve_approval_remotely).
    pub fn resolve_clarification_remotely(
        &self,
        id: &str,
        answers: Vec<ClarificationAnswer>,
    ) -> impl Future<Output = ()> + use<> {
        let broker = self.answer_broker_input(id, InputAnswer::Answers(answers.clone()));
        let sends: Vec<_> = self
            .hosted
            .values()
            .filter(|remote| remote.is_turn_active())
            .map(|remote| remote.resolve_clarification(id, answers.clone()))
            .collect();
        async move {
            match broker {
                Some(broker) => broker.await,
                None => {
                    futures::future::join_all(sends).await;
                }
            }
        }
    }

    /// Remove a conversation from both the in-memory cache and the metadata list.
    /// Returns true if the conversation existed in either.
    pub fn delete_conversation(&mut self, id: &str) -> bool {
        let in_cache = self.sessions.remove(id).is_some();
        self.hosted.remove(id);
        let in_metadata = self.metadata.iter().any(|m| m.id == id);
        self.remove_metadata(id);
        self.access_order.retain(|s| s != id);
        self.streaming_ids.remove(id);

        if self.active_conversation_id.as_deref() == Some(id) {
            self.active_conversation_id = self.metadata.first().map(|m| m.id.clone());
        }

        in_cache || in_metadata
    }

    // ── Active conversation ───────────────────────────────────────────────────

    /// Set the active conversation ID unconditionally (does not validate against metadata).
    ///
    /// Use this when the conversation is known to exist — e.g., immediately after creating
    /// or lazy-loading it, before the metadata list has been updated. Prefer `set_active`
    /// when you want existence validation.
    pub fn set_active_by_id(&mut self, id: String) {
        self.finished.remove(&id);
        self.failed.remove(&id);
        self.touch_access_order(&id);
        self.active_conversation_id = Some(id);
    }

    /// Set active only if the conversation exists in the metadata list; returns false otherwise.
    ///
    /// Prefer this over `set_active_by_id` when the ID comes from an external source and
    /// you want to guard against setting a stale or invalid active conversation.
    pub fn set_active(&mut self, id: String) -> bool {
        if self.metadata.iter().any(|m| m.id == id) {
            self.active_conversation_id = Some(id);
            true
        } else {
            false
        }
    }

    /// Get the active conversation ID.
    pub fn active_id(&self) -> Option<&String> {
        self.active_conversation_id.as_ref()
    }
}

impl Default for ConversationsStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store_with_n_entries(n: usize) -> ConversationsStore {
        let mut store = ConversationsStore::new();
        for i in 0..n {
            store.upsert_metadata(
                &format!("conv-{i}"),
                &format!("Title {i}"),
                0.0,
                i as i64, // updated_at: conv-0 is oldest, conv-(n-1) is newest
            );
        }
        store
    }

    /// Simulate inserting a conversation into the LRU tracker without needing
    /// a real Conversation object (which requires an AgentClient).
    fn insert_dummy(store: &mut ConversationsStore, id: &str) {
        store.access_order.retain(|s| s != id);
        store.access_order.push_back(id.to_string());
    }

    fn cached_ids(store: &ConversationsStore) -> Vec<String> {
        store.access_order.iter().cloned().collect()
    }

    /// HX-14: this device's id is cached per sign-in, so a new sign-in (or
    /// signing out) forgets it.
    #[test]
    fn this_devices_id_is_forgotten_when_the_sign_in_changes() {
        let mut store = ConversationsStore::new();
        store.set_this_device(Some("s-1".into()));
        assert_eq!(store.this_device().as_deref(), Some("s-1"));
        store.set_hosted_auth(Some(Arc::new(HiveSession::new(
            "https://hive.example".to_string(),
            None,
        ))));
        assert_eq!(store.this_device(), None);
    }

    #[test]
    fn list_recent_metadata_returns_correct_count() {
        let store = make_store_with_n_entries(100);
        assert_eq!(store.list_recent_metadata(20).len(), 20);
        assert_eq!(store.list_recent_metadata(100).len(), 100);
        assert_eq!(store.list_recent_metadata(200).len(), 100); // capped at total
    }

    #[test]
    fn list_recent_metadata_is_sorted_most_recent_first() {
        let store = make_store_with_n_entries(50);
        let recent = store.list_recent_metadata(10);
        assert_eq!(recent.len(), 10);
        // Newest entries have the highest updated_at; upsert assigns updated_at = i,
        // so conv-49 is first, conv-48 is second, etc.
        assert_eq!(recent[0].0, "conv-49");
        assert_eq!(recent[1].0, "conv-48");
        assert_eq!(recent[9].0, "conv-40");
    }

    #[test]
    fn upsert_metadata_updates_existing_entry_and_re_sorts() {
        let mut store = make_store_with_n_entries(5);
        // conv-0 has updated_at=0 (oldest). Update it to be the newest.
        store.upsert_metadata("conv-0", "Title 0 updated", 1.5, 999);
        let all = store.list_recent_metadata(5);
        assert_eq!(all[0].0, "conv-0"); // conv-0 is now first (most recent)
    }

    #[test]
    fn all_metadata_ids_returns_all_ids_most_recent_first() {
        let store = make_store_with_n_entries(1000);
        let ids = store.all_metadata_ids();
        assert_eq!(ids.len(), 1000);
        assert_eq!(ids[0], "conv-999");
        assert_eq!(ids[999], "conv-0");
    }

    // ── LRU eviction tests ──────────────────────────────────────────────────

    #[test]
    fn touch_access_order_moves_to_back() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");
        insert_dummy(&mut store, "b");
        insert_dummy(&mut store, "c");
        assert_eq!(cached_ids(&store), vec!["a", "b", "c"]);

        // Touch "a" — should move to back
        store.touch_access_order("a");
        assert_eq!(cached_ids(&store), vec!["b", "c", "a"]);
    }

    #[test]
    fn find_lru_evictable_returns_oldest_non_protected() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");
        insert_dummy(&mut store, "b");
        insert_dummy(&mut store, "c");

        // No protections: oldest is "a"
        assert_eq!(store.find_lru_evictable(), Some("a".to_string()));

        // Protect "a" as active: oldest evictable is "b"
        store.set_active_by_id("a".to_string());
        assert_eq!(store.find_lru_evictable(), Some("b".to_string()));

        // Also protect "b" as streaming: oldest evictable is "c"
        store.mark_streaming("b");
        assert_eq!(store.find_lru_evictable(), Some("c".to_string()));

        // Protect "c" too: nothing evictable
        store.mark_streaming("c");
        assert_eq!(store.find_lru_evictable(), None);
    }

    #[test]
    fn set_active_by_id_updates_access_order() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");
        insert_dummy(&mut store, "b");
        insert_dummy(&mut store, "c");

        // Selecting "a" as active moves it to the back
        store.set_active_by_id("a".to_string());
        assert_eq!(cached_ids(&store), vec!["b", "c", "a"]);
    }

    #[test]
    fn mark_streaming_protects_from_eviction() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");
        insert_dummy(&mut store, "b");

        store.mark_streaming("a");
        assert_eq!(store.find_lru_evictable(), Some("b".to_string()));

        store.unmark_streaming("a");
        assert_eq!(store.find_lru_evictable(), Some("a".to_string()));
    }

    #[test]
    fn delete_conversation_cleans_up_access_order_and_streaming() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");
        insert_dummy(&mut store, "b");
        store.mark_streaming("a");

        store.delete_conversation("a");
        assert_eq!(cached_ids(&store), vec!["b"]);
        // streaming_ids should also be cleaned up
        assert_eq!(store.find_lru_evictable(), Some("b".to_string()));
    }

    /// The sidebar badges a hosted conversation from the metadata row alone,
    /// so no conversation has to be loaded to know where it runs (AGE-298).
    #[test]
    fn is_hosted_is_answered_from_the_metadata_row_without_loading() {
        let mut store = make_store_with_n_entries(2);
        store
            .metadata
            .iter_mut()
            .find(|m| m.id == "conv-1")
            .unwrap()
            .sync = ConversationSync::cloud("http://localhost:8081", "r-1", None);

        assert!(store.is_hosted("conv-1"));
        assert!(!store.is_hosted("conv-0"));
        assert!(!store.is_hosted("never-seen"));
    }

    /// The hosted client follows the placement: a move online creates it, a
    /// move back removes it, and a delete never leaves one behind. Both maps
    /// are written from the same call so they cannot disagree about where a
    /// conversation runs.
    #[test]
    fn the_hosted_client_follows_the_conversation_placement() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");

        store.set_conversation_sync(
            "a",
            ConversationSync::cloud("http://localhost:8081/", "r-1", None),
        );
        let hosted = store
            .get_hosted("a")
            .expect("a hosted conversation has a client");
        assert_eq!(hosted.server_url(), "http://localhost:8081");
        assert_eq!(hosted.remote_id(), "r-1");

        store.set_conversation_sync("a", ConversationSync::LocalOnly);
        assert!(store.get_hosted("a").is_none(), "back home, no client");

        store.set_conversation_sync(
            "a",
            ConversationSync::cloud("http://localhost:8081", "r-2", None),
        );
        store.delete_conversation("a");
        assert!(store.get_hosted("a").is_none(), "deleted, no client");
    }

    /// A pull reaches the sidebar rows and the hosted clients, but never
    /// deletes the conversation the user has open.
    #[test]
    fn sync_actions_update_rows_and_spare_the_open_conversation() {
        use crate::services::hive_sync::{RemoteRow, SyncAction, TurnStatus};

        let mut store = make_store_with_n_entries(3);
        store.set_active("conv-2".to_string());
        let cloud = ConversationSync::cloud("https://hive.example", "r-9", None);
        let kept = store.apply_sync_actions(&[
            SyncAction::UpsertMetadata(RemoteRow {
                local_id: "r-9".into(),
                title: "From the web".into(),
                model_id: None,
                updated_at: 100,
                sync: cloud.clone(),
                turn_status: TurnStatus::Running,
                device_name: None,
            }),
            SyncAction::ApplyTombstone {
                local_id: "conv-0".into(),
            },
            SyncAction::ApplyTombstone {
                local_id: "conv-2".into(),
            },
        ]);

        assert_eq!(
            kept.len(),
            2,
            "the open conversation's tombstone is held back"
        );
        assert_eq!(store.all_metadata_ids(), vec!["r-9", "conv-2", "conv-1"]);
        assert!(store.is_hosted("r-9"));
        assert_eq!(store.metadata()[0].sync, cloud);
    }

    #[test]
    fn max_cached_conversations_constant_is_reasonable() {
        // Guard: keep the constant between 5 and 50 to prevent accidental extremes
        const {
            assert!(
                MAX_CACHED_CONVERSATIONS >= 5,
                "Cache limit too low — would cause excessive reloads"
            );
            assert!(
                MAX_CACHED_CONVERSATIONS <= 50,
                "Cache limit too high — defeats the purpose of eviction"
            );
        }
    }

    /// HX-10: the sidebar's mark follows the pull's status, a finished turn
    /// stays marked until the conversation is opened, and a local-only row
    /// never has one.
    #[test]
    fn the_cloud_mark_follows_the_turn_and_clears_when_opened() {
        let mut store = make_store_with_n_entries(2);
        store.set_active("conv-0".to_string());
        store.set_conversation_sync(
            "conv-1",
            ConversationSync::cloud("https://hive.example", "r-1", None),
        );
        assert_eq!(store.cloud_mark("conv-1"), None);

        store.set_turn_status("conv-1", TurnStatus::Running);
        assert_eq!(store.cloud_mark("conv-1"), Some(CloudMark::Running));
        store.set_turn_status("conv-1", TurnStatus::WaitingForYou);
        assert_eq!(store.cloud_mark("conv-1"), Some(CloudMark::WaitingForYou));
        store.set_turn_status("conv-1", TurnStatus::Idle);
        assert_eq!(store.cloud_mark("conv-1"), Some(CloudMark::Done));

        store.set_active_by_id("conv-1".to_string());
        assert_eq!(
            store.cloud_mark("conv-1"),
            None,
            "opening it clears the mark"
        );

        store.set_turn_status("conv-0", TurnStatus::Running);
        assert_eq!(
            store.cloud_mark("conv-0"),
            None,
            "local-only rows have none"
        );
    }

    /// HX-11: only an id remembered as a broker input is answered on the
    /// input route; anything else falls through to the conversation's own.
    #[test]
    fn only_remembered_broker_inputs_take_the_input_route() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");
        store.set_conversation_sync(
            "a",
            ConversationSync::cloud("http://127.0.0.1:1", "r-1", None),
        );
        assert!(
            store
                .answer_broker_input("approval-1", InputAnswer::Approval(true))
                .is_none()
        );
        let nonce = uuid::Uuid::new_v4().to_string();
        store.remember_input("a", "approval-1", &nonce);
        assert!(
            store
                .answer_broker_input("approval-1", InputAnswer::Approval(true))
                .is_some()
        );
        store.forget_input("approval-1");
        assert!(
            store
                .answer_broker_input("approval-1", InputAnswer::Approval(true))
                .is_none()
        );
    }
}
