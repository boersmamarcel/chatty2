use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use super::error::RepositoryResult;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Default empty token usage for backward compatibility
fn default_empty_token_usage() -> String {
    "{}".to_string()
}

/// Default empty attachment paths for backward compatibility
fn default_empty_attachments() -> String {
    "[]".to_string()
}

/// Serializable conversation data for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationData {
    pub id: String,
    pub title: String,
    pub model_id: String,
    pub message_history: String, // JSON-serialized Vec<Message>
    pub system_traces: String,   // JSON-serialized Vec<Option<serde_json::Value>>
    #[serde(default = "default_empty_token_usage")]
    pub token_usage: String, // JSON-serialized ConversationTokenUsage
    #[serde(default = "default_empty_attachments")]
    pub attachment_paths: String, // JSON-serialized Vec<Vec<String>> (per-message file paths)
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
