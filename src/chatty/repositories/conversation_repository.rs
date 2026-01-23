use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type RepositoryResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Serializable conversation data for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationData {
    pub id: String,
    pub title: String,
    pub model_id: String,
    pub message_history: String, // JSON-serialized Vec<Message>
    pub system_traces: String,   // JSON-serialized Vec<Option<serde_json::Value>>
    pub created_at: i64,         // Unix timestamp
    pub updated_at: i64,         // Unix timestamp
}

/// Repository trait for conversation persistence
pub trait ConversationRepository: Send + Sync + 'static {
    /// Load all conversations from storage
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>>;

    /// Save a conversation to storage
    fn save(&self, id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Delete a conversation from storage
    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>>;
}
