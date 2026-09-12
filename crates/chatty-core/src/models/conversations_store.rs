use std::collections::{HashMap, HashSet, VecDeque};

use crate::models::clarification_store::ClarificationAnswer;
use crate::models::execution_approval_store::ApprovalDecision;
use crate::models::write_approval_store::WriteApprovalDecision;
use crate::repositories::ConversationMetadata;
use crate::session::{AgentSession, HostedSession};

use super::conversation::{Conversation, ConversationMode};

/// The mode as the row and the metadata layer carry it: `None` for Local, so
/// a conversation that never moves is stored exactly as it was before AGE-298.
fn serialize_mode(mode: &ConversationMode) -> Option<String> {
    match mode {
        ConversationMode::Local => None,
        hosted => serde_json::to_string(hosted).ok(),
    }
}

/// Maximum number of full conversation objects kept in memory.
/// When the cache exceeds this limit, the least recently used conversations are evicted.
/// They will be re-loaded from SQLite on demand if the user navigates back.
const MAX_CACHED_CONVERSATIONS: usize = 10;

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
    /// The hosted client for each loaded conversation whose mode is
    /// `Hosted` (AGE-298). Keyed like `sessions` and derived from the
    /// conversation's own mode when it is loaded, so the two cannot disagree
    /// about where a conversation runs. A conversation always has a session;
    /// it has an entry here as well only while it runs elsewhere.
    hosted: HashMap<String, HostedSession>,
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

    /// Does this conversation run on a server (AGE-298)?
    ///
    /// Answered from the metadata layer, so the sidebar can badge a hosted
    /// conversation without loading it — which is the point of that layer.
    /// A loaded conversation is asked directly, because it is the fresher of
    /// the two right after a move.
    pub fn is_hosted(&self, id: &str) -> bool {
        if let Some(conversation) = self.get_conversation(id) {
            return conversation.mode().is_hosted();
        }
        self.metadata
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.mode.as_deref())
            .and_then(|json| serde_json::from_str::<ConversationMode>(json).ok())
            .is_some_and(|mode| mode.is_hosted())
    }

    /// Insert or update a single metadata entry and re-sort by updated_at descending.
    pub fn upsert_metadata(&mut self, id: &str, title: &str, total_cost: f64, updated_at: i64) {
        // The mode is not a parameter: every caller of this is reporting a
        // title/cost change after a turn, and none of them knows where the
        // conversation runs. It is set by a move, through
        // [`set_conversation_mode`](Self::set_conversation_mode), and read
        // back from the row on the next load. The turn totals (AGE-351) are
        // taken from the loaded conversation the same way: the session
        // accumulates them at the turn barrier, and the row carries them.
        let loaded = self.get_conversation(id).map(|conversation| {
            (
                conversation.mode().clone(),
                conversation.tool_call_count(),
                conversation.context_tokens(),
            )
        });
        if let Some(entry) = self.metadata.iter_mut().find(|m| m.id == id) {
            entry.title = title.to_string();
            entry.total_cost = total_cost;
            entry.updated_at = updated_at;
            if let Some((mode, tool_call_count, context_tokens)) = loaded {
                entry.mode = serialize_mode(&mode);
                entry.tool_call_count = tool_call_count;
                entry.context_tokens = context_tokens;
            }
        } else {
            let (mode, tool_call_count, context_tokens) = loaded.unwrap_or_default();
            self.metadata.push(ConversationMetadata {
                id: id.to_string(),
                title: title.to_string(),
                total_cost,
                updated_at,
                mode: serialize_mode(&mode),
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
        // The hosted client is derived from the conversation's own mode, here
        // and nowhere else, so "where the row says it runs" and "where turns
        // actually go" cannot drift apart (AGE-298).
        match session.conversation().map(|c| c.mode().clone()) {
            Some(ConversationMode::Hosted {
                server_url,
                remote_id,
            }) => {
                self.hosted
                    .insert(id.clone(), HostedSession::new(server_url, remote_id));
            }
            _ => {
                self.hosted.remove(&id);
            }
        }
        self.sessions.insert(id.clone(), session);
        self.touch_access_order(&id);
        self.evict_if_needed();
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

    pub fn get_hosted_mut(&mut self, id: &str) -> Option<&mut HostedSession> {
        self.hosted.get_mut(id)
    }

    /// Record where a conversation runs after a move completed.
    ///
    /// Called with the conversation's new mode *after* the history transfer
    /// succeeded — never before, so a failed move leaves both the row and this
    /// map describing the place the conversation is still running.
    pub fn set_conversation_mode(&mut self, id: &str, mode: ConversationMode) {
        match &mode {
            ConversationMode::Hosted {
                server_url,
                remote_id,
            } => {
                self.hosted
                    .insert(id.to_string(), HostedSession::new(server_url, remote_id));
            }
            ConversationMode::Local => {
                self.hosted.remove(id);
            }
        }
        if let Some(conversation) = self.get_conversation_mut(id) {
            conversation.set_mode(mode);
        }
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
                // The hosted client is rebuilt from the conversation's mode
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

    /// Number of full conversations currently cached in memory.
    pub fn cached_count(&self) -> usize {
        self.sessions.len()
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
        let sends: Vec<_> = self
            .hosted
            .values()
            .filter(|remote| remote.is_turn_active())
            .map(|remote| remote.resolve_approval(id, approved))
            .collect();
        async move {
            futures::future::join_all(sends).await;
        }
    }

    /// Answer an `ask_user` raised by a hosted conversation's agent; see
    /// [`resolve_approval_remotely`](Self::resolve_approval_remotely).
    pub fn resolve_clarification_remotely(
        &self,
        id: &str,
        answers: Vec<ClarificationAnswer>,
    ) -> impl Future<Output = ()> + use<> {
        let sends: Vec<_> = self
            .hosted
            .values()
            .filter(|remote| remote.is_turn_active())
            .map(|remote| remote.resolve_clarification(id, answers.clone()))
            .collect();
        async move {
            futures::future::join_all(sends).await;
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
        self.touch_access_order(&id);
        self.active_conversation_id = Some(id);
    }

    /// Set active only if the conversation exists in the metadata list; returns false otherwise.
    ///
    /// Prefer this over `set_active_by_id` when the ID comes from an external source and
    /// you want to guard against setting a stale or invalid active conversation.
    #[allow(dead_code)]
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
            .mode = Some(
            r#"{"kind":"hosted","server_url":"http://localhost:8081","remote_id":"r-1"}"#
                .to_string(),
        );

        assert!(store.is_hosted("conv-1"));
        assert!(!store.is_hosted("conv-0"));
        assert!(!store.is_hosted("never-seen"));
    }

    /// The hosted client follows the mode: a move online creates it, a move
    /// back removes it, and a delete never leaves one behind. Both maps are
    /// written from the same call so they cannot disagree about where a
    /// conversation runs.
    #[test]
    fn the_hosted_client_follows_the_conversation_mode() {
        let mut store = ConversationsStore::new();
        insert_dummy(&mut store, "a");

        store.set_conversation_mode(
            "a",
            ConversationMode::Hosted {
                server_url: "http://localhost:8081/".to_string(),
                remote_id: "r-1".to_string(),
            },
        );
        let hosted = store
            .get_hosted("a")
            .expect("a hosted conversation has a client");
        assert_eq!(hosted.server_url(), "http://localhost:8081");
        assert_eq!(hosted.remote_id(), "r-1");

        store.set_conversation_mode("a", ConversationMode::Local);
        assert!(store.get_hosted("a").is_none(), "back home, no client");

        store.set_conversation_mode(
            "a",
            ConversationMode::Hosted {
                server_url: "http://localhost:8081".to_string(),
                remote_id: "r-2".to_string(),
            },
        );
        store.delete_conversation("a");
        assert!(store.get_hosted("a").is_none(), "deleted, no client");
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
}
