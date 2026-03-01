use gpui::Global;
use std::collections::HashMap;

use crate::chatty::repositories::ConversationMetadata;

use super::conversation::Conversation;

/// Global store for all conversations.
///
/// Two-layer design:
/// - `metadata`: always loaded at startup (lightweight — just id/title/cost)
/// - `conversations`: lazily populated when a conversation is selected
pub struct ConversationsStore {
    /// Lightweight metadata list, sorted by updated_at descending.
    /// This is the source of truth for the sidebar and navigation.
    metadata: Vec<ConversationMetadata>,
    /// Full conversation data, populated on demand when the user selects a conversation.
    conversations: HashMap<String, Conversation>,
    active_conversation_id: Option<String>,
}

impl Global for ConversationsStore {}

impl ConversationsStore {
    pub fn new() -> Self {
        Self {
            metadata: Vec::new(),
            conversations: HashMap::new(),
            active_conversation_id: None,
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
    pub fn insert_loaded(&mut self, conversation: Conversation) {
        let id = conversation.id().to_string();
        self.conversations.insert(id, conversation);
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
    #[allow(dead_code)]
    pub fn list_recent(&self, limit: usize) -> Vec<&Conversation> {
        let mut convs: Vec<&Conversation> = self.conversations.values().collect();
        convs.sort_by_key(|c| std::cmp::Reverse(c.updated_at()));
        convs.truncate(limit);
        convs
    }
}

impl Default for ConversationsStore {
    fn default() -> Self {
        Self::new()
    }
}
