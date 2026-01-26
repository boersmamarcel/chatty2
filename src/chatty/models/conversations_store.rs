use gpui::Global;
use std::collections::HashMap;

use super::conversation::Conversation;

/// Global store for all conversations
pub struct ConversationsModel {
    conversations: HashMap<String, Conversation>,
    active_conversation_id: Option<String>,
}

impl Global for ConversationsModel {}

impl ConversationsModel {
    pub fn new() -> Self {
        Self {
            conversations: HashMap::new(),
            active_conversation_id: None,
        }
    }

    /// Add a conversation to the store
    pub fn add_conversation(&mut self, conversation: Conversation) {
        let id = conversation.id().to_string();
        self.conversations.insert(id.clone(), conversation);

        // Set as active if it's the first conversation
        if self.active_conversation_id.is_none() {
            self.active_conversation_id = Some(id);
        }
    }

    /// Get a conversation by ID (immutable)
    pub fn get_conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.get(id)
    }

    /// Get a mutable reference to a conversation by ID
    pub fn get_conversation_mut(&mut self, id: &str) -> Option<&mut Conversation> {
        self.conversations.get_mut(id)
    }

    /// Delete a conversation by ID
    pub fn delete_conversation(&mut self, id: &str) -> bool {
        let removed = self.conversations.remove(id).is_some();

        // If we deleted the active conversation, switch to another or none
        if self.active_conversation_id.as_deref() == Some(id) {
            self.active_conversation_id = self.conversations.keys().next().cloned();
        }

        removed
    }

    /// Set the active conversation
    pub fn set_active(&mut self, id: String) -> bool {
        if self.conversations.contains_key(&id) {
            self.active_conversation_id = Some(id);
            true
        } else {
            false
        }
    }

    /// Get the active conversation ID
    pub fn active_id(&self) -> Option<&String> {
        self.active_conversation_id.as_ref()
    }

    /// List all conversations (sorted by updated_at descending)
    pub fn list_all(&self) -> Vec<&Conversation> {
        let mut convs: Vec<&Conversation> = self.conversations.values().collect();
        convs.sort_by_key(|c| std::cmp::Reverse(c.updated_at()));
        convs
    }

    /// Get count of conversations
    pub fn count(&self) -> usize {
        self.conversations.len()
    }
}

impl Default for ConversationsModel {
    fn default() -> Self {
        Self::new()
    }
}
