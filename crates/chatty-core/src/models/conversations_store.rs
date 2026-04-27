use std::collections::{HashMap, HashSet, VecDeque};

use crate::repositories::ConversationMetadata;

use super::conversation::Conversation;

/// Maximum number of full conversation objects kept in memory.
/// When the cache exceeds this limit, the least recently used conversations are evicted.
/// They will be re-loaded from SQLite on demand if the user navigates back.
const MAX_CACHED_CONVERSATIONS: usize = 10;

/// Global store for all conversations.
///
/// Two-layer design:
/// - `metadata`: always loaded at startup (lightweight — just id/title/cost)
/// - `conversations`: lazily populated when a conversation is selected, with LRU eviction
pub struct ConversationsStore {
    /// Lightweight metadata list, sorted by updated_at descending.
    /// This is the source of truth for the sidebar and navigation.
    metadata: Vec<ConversationMetadata>,
    /// Full conversation data, populated on demand when the user selects a conversation.
    /// Bounded to `MAX_CACHED_CONVERSATIONS` entries via LRU eviction.
    conversations: HashMap<String, Conversation>,
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
            conversations: HashMap::new(),
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

    /// Insert or update a single metadata entry and re-sort by updated_at descending.
    pub fn upsert_metadata(&mut self, id: &str, title: &str, total_cost: f64, updated_at: i64) {
        if let Some(entry) = self.metadata.iter_mut().find(|m| m.id == id) {
            entry.title = title.to_string();
            entry.total_cost = total_cost;
            entry.updated_at = updated_at;
        } else {
            self.metadata.push(ConversationMetadata {
                id: id.to_string(),
                title: title.to_string(),
                total_cost,
                updated_at,
            });
        }
        // Keep sorted: most recently updated first
        self.metadata
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    }

    /// Remove a conversation from the metadata list.
    pub fn remove_metadata(&mut self, id: &str) {
        self.metadata.retain(|m| m.id != id);
    }

    // ── Full conversation cache ───────────────────────────────────────────────

    /// Returns true if the full conversation data is already in memory.
    pub fn is_loaded(&self, id: &str) -> bool {
        self.conversations.contains_key(id)
    }

    /// Insert a lazily-loaded conversation into the cache.
    /// Evicts the least recently used non-active, non-streaming conversations if the cache
    /// exceeds `MAX_CACHED_CONVERSATIONS`.
    pub fn insert_loaded(&mut self, conversation: Conversation) {
        let id = conversation.id().to_string();
        self.conversations.insert(id.clone(), conversation);
        self.touch_access_order(&id);
        self.evict_if_needed();
    }

    /// Move an ID to the back of the access order (most recently used).
    fn touch_access_order(&mut self, id: &str) {
        self.access_order.retain(|s| s != id);
        self.access_order.push_back(id.to_string());
    }

    /// Evict least recently used non-protected conversations when cache exceeds the limit.
    /// Protected conversations: the active one and any with active streams.
    fn evict_if_needed(&mut self) {
        while self.conversations.len() > MAX_CACHED_CONVERSATIONS {
            let evict_id = self.find_lru_evictable();
            if let Some(id) = evict_id {
                self.conversations.remove(&id);
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
    pub fn unmark_streaming(&mut self, id: &str) {
        self.streaming_ids.remove(id);
    }

    /// Number of full conversations currently cached in memory.
    pub fn cached_count(&self) -> usize {
        self.conversations.len()
    }

    /// Get a conversation by ID (immutable). Returns `None` if not yet loaded.
    pub fn get_conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.get(id)
    }

    /// Get a mutable reference to a conversation by ID.
    pub fn get_conversation_mut(&mut self, id: &str) -> Option<&mut Conversation> {
        self.conversations.get_mut(id)
    }

    /// Remove a conversation from both the in-memory cache and the metadata list.
    /// Returns true if the conversation existed in either.
    pub fn delete_conversation(&mut self, id: &str) -> bool {
        let in_cache = self.conversations.remove(id).is_some();
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

    /// Clear the active conversation.
    #[allow(dead_code)]
    pub fn clear_active(&mut self) {
        self.active_conversation_id = None;
    }

    // ── Legacy helpers (kept for compatibility) ───────────────────────────────

    /// List the N most recent conversations from the in-memory cache.
    /// Prefer `list_recent_metadata()` for sidebar display.
    ///
    /// Uses O(n) average selection via `select_nth_unstable_by` to find the top-K
    /// without sorting the entire collection, then sorts only the K results.
    /// Overall complexity: O(n + K log K) instead of O(n log n).
    #[allow(dead_code)]
    pub fn list_recent(&self, limit: usize) -> Vec<&Conversation> {
        let mut convs: Vec<&Conversation> = self.conversations.values().collect();
        if convs.len() > limit {
            convs.select_nth_unstable_by(limit, |a, b| {
                b.updated_at().cmp(&a.updated_at()) // descending: largest first
            });
            convs.truncate(limit);
        }
        convs.sort_by_key(|c| std::cmp::Reverse(c.updated_at()));
        convs
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

    #[test]
    fn max_cached_conversations_constant_is_reasonable() {
        // Guard: keep the constant between 5 and 50 to prevent accidental extremes
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
