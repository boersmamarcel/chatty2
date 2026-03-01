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

/// Default empty message timestamps for backward compatibility
fn default_empty_timestamps() -> String {
    "[]".to_string()
}

/// Default empty message feedback for backward compatibility
fn default_empty_feedback() -> String {
    "[]".to_string()
}

/// Default empty regeneration records for backward compatibility
fn default_empty_regeneration_records() -> String {
    "[]".to_string()
}

/// Lightweight conversation metadata used for the sidebar.
/// Loaded at startup without deserializing full message history.
#[derive(Debug, Clone)]
pub struct ConversationMetadata {
    pub id: String,
    pub title: String,
    pub total_cost: f64,
    pub updated_at: i64,
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
    #[serde(default = "default_empty_timestamps")]
    pub message_timestamps: String, // JSON-serialized Vec<Option<i64>> (per-message Unix timestamps)
    #[serde(default = "default_empty_feedback")]
    pub message_feedback: String, // JSON-serialized Vec<Option<MessageFeedback>> (per-message feedback)
    #[serde(default = "default_empty_regeneration_records")]
    pub regeneration_records: String, // JSON-serialized Vec<RegenerationRecord> (DPO preference pairs)
    pub created_at: i64, // Unix timestamp
    pub updated_at: i64, // Unix timestamp
}

impl ConversationData {
    /// Extract `total_estimated_cost_usd` from the JSON-serialized `token_usage` field.
    pub fn total_cost(&self) -> f64 {
        serde_json::from_str::<serde_json::Value>(&self.token_usage)
            .ok()
            .and_then(|v| v.get("total_estimated_cost_usd").and_then(|c| c.as_f64()))
            .unwrap_or(0.0)
    }
}

/// Repository trait for conversation persistence
pub trait ConversationRepository: Send + Sync + 'static {
    /// Load lightweight metadata for all conversations (fast — no message deserialization)
    fn load_metadata(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationMetadata>>>;

    /// Load full data for a single conversation by ID
    fn load_one(&self, id: &str) -> BoxFuture<'static, RepositoryResult<Option<ConversationData>>>;

    /// Load all conversations from storage (kept for compatibility/export use cases)
    #[allow(dead_code)]
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>>;

    /// Save a conversation to storage
    fn save(&self, id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Delete a conversation from storage
    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>>;
}
