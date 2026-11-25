use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use super::error::RepositoryResult;
use crate::models::conversation::ConversationSync;

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

/// Default None working directory for backward compatibility
fn default_none_working_dir() -> Option<String> {
    None
}

/// Default None agent task snapshot for backward compatibility
fn default_none_agent_task_snapshot() -> Option<String> {
    None
}

/// Lightweight conversation metadata used for the sidebar.
/// Loaded at startup without deserializing full message history.
#[derive(Debug, Clone)]
pub struct ConversationMetadata {
    pub id: String,
    pub title: String,
    pub total_cost: f64,
    pub updated_at: i64,
    /// Whether the conversation is synced and where its turns run (AGE-2088),
    /// read from the row's sync columns.
    ///
    /// Carried in the lightweight layer because the sidebar has to badge a
    /// synced or cloud conversation *without* loading it, and because the
    /// sync engine matches the server's list against these rows — the whole
    /// point of this layer is that neither costs a message deserialization.
    pub sync: ConversationSync,
    /// Tool calls made over the conversation's lifetime (AGE-351).
    pub tool_call_count: u32,
    /// Prompt size of the last completed API call — the context fill the
    /// next request starts from (AGE-351).
    pub context_tokens: u32,
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
    #[serde(default = "default_none_working_dir")]
    pub working_dir: Option<String>, // Per-conversation working directory override
    #[serde(default = "default_none_agent_task_snapshot")]
    pub agent_task_snapshot: Option<String>, // JSON-serialized AgentTaskSnapshot
    /// The sync columns (AGE-2088), read and written through
    /// [`ConversationSync::from_columns`] / [`ConversationSync::to_columns`].
    /// All `None` is local-only, so a conversation that never syncs keeps the
    /// bytes it had before sync existed.
    ///
    /// `sync`: JSON `{server_url, remote_id}`.
    #[serde(default)]
    pub sync: Option<String>,
    /// JSON [`Placement`](crate::models::conversation::Placement).
    #[serde(default)]
    pub placement: Option<String>,
    /// The server version this device's copy of the history is at.
    #[serde(default)]
    pub remote_version: Option<i64>,
    /// When (unix ms) this device last brought its copy in line with the
    /// server; `None` while the copy is stale or was never fetched.
    #[serde(default)]
    pub last_synced_at: Option<i64>,
    /// Tool calls made over the conversation's lifetime, accumulated at the
    /// turn barrier; a row from before the column reports 0 (AGE-351).
    #[serde(default)]
    pub tool_call_count: u32,
    /// Prompt size of the last completed API call (`input + cache_read +
    /// cache_write`, output excluded); a row from before the column reports
    /// 0 (AGE-351).
    #[serde(default)]
    pub context_tokens: u32,
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
    ///
    /// Ordered newest-`updated_at` first. This layer renders the sidebar, so
    /// the order is part of the contract and every implementation must honour
    /// it; the conformance suite asserts it. `load_all` carries no such
    /// promise.
    fn load_metadata(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationMetadata>>>;

    /// Load full data for a single conversation by ID
    fn load_one(&self, id: &str) -> BoxFuture<'static, RepositoryResult<Option<ConversationData>>>;

    /// Load all conversations from storage (kept for compatibility/export use cases)
    ///
    /// No ordering is promised. Callers that need conversations newest-first
    /// want `load_metadata`, or must sort for themselves.
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>>;

    /// Save a conversation to storage
    fn save(&self, id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Delete a conversation from storage
    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>>;
}
