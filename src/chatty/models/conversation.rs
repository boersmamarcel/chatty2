use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::trace;

use rig::OneOrMany;
use rig::completion::Message;
use rig::completion::message::{AssistantContent, Text};

use crate::chatty::factories::AgentClient;
use crate::chatty::models::token_usage::{ConversationTokenUsage, TokenUsage};
use crate::chatty::repositories::ConversationData;
use crate::chatty::services::shell_service::ShellSession;
use crate::chatty::tools::PendingArtifacts;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;

/// User feedback signal for an individual assistant message
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessageFeedback {
    ThumbsUp,
    ThumbsDown,
}

/// Record of a regenerated assistant response, capturing the original text
/// for DPO (Direct Preference Optimization) preference pair training data.
/// The original text is the "rejected" response; the replacement is the "chosen" response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegenerationRecord {
    /// Index into the conversation history identifying which assistant message was regenerated
    pub message_index: usize,
    /// The full text of the original (rejected) assistant response before replacement
    pub original_text: String,
    /// Unix timestamp (seconds) when the original response was generated
    pub original_timestamp: i64,
    /// Unix timestamp (seconds) when the regeneration was requested
    pub regeneration_timestamp: i64,
}

/// A single conversation with an AI agent
pub struct Conversation {
    id: String,
    title: String,
    model_id: String,
    agent: AgentClient,
    // ── Parallel arrays ──────────────────────────────────────────────
    // The following Vecs are all parallel to `history` (one entry per message).
    // Every push to `history` must be accompanied by a push to each Vec.
    //
    // Note: `system_traces` is pushed in two places — `add_user_message_with_attachments`
    // (pushes None for user messages) and `add_trace` (pushes the real trace for
    // assistant messages). The caller must always call `add_trace` immediately after
    // `finalize_response` to maintain the invariant.
    history: Vec<Message>,
    system_traces: Vec<Option<serde_json::Value>>,
    attachment_paths: Vec<Vec<PathBuf>>,
    message_timestamps: Vec<Option<i64>>,
    message_feedback: Vec<Option<MessageFeedback>>,
    // ── End parallel arrays ──────────────────────────────────────────
    /// Regeneration records capturing original responses before replacement (DPO preference pairs)
    regeneration_records: Vec<RegenerationRecord>,
    token_usage: ConversationTokenUsage,
    created_at: SystemTime,
    updated_at: SystemTime,
    /// Partial streaming message being composed (None if no active stream)
    streaming_message: Option<String>,
    /// Shared state for artifacts queued by AddAttachmentTool during a stream
    pending_artifacts: PendingArtifacts,
    /// Persistent shell session for this conversation (lazily initialized)
    shell_session: Option<std::sync::Arc<ShellSession>>,
}

impl Conversation {
    /// Create a new conversation from model and provider config
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        id: String,
        title: String,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
        exec_settings: Option<crate::settings::models::ExecutionSettingsModel>,
        pending_approvals: Option<
            crate::chatty::models::execution_approval_store::PendingApprovals,
        >,
        pending_write_approvals: Option<
            crate::chatty::models::write_approval_store::PendingWriteApprovals,
        >,
    ) -> Result<Self> {
        // Log URL information
        let url_info = provider_config
            .base_url
            .as_ref()
            .map(|url| format!(" with URL: {}", url))
            .unwrap_or_else(|| " (using default URL)".to_string());
        trace!(
            "Initializing conversation with provider: {:?}{}, model: {}",
            provider_config.provider_type, url_info, model_config.model_identifier
        );

        let pending_artifacts: PendingArtifacts =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        // Shell session is created inside the factory when execution is enabled.
        // The factory returns it so we can store it on the Conversation for reuse
        // across agent rebuilds (MCP changes, model switches).
        let (agent, shell_session) = AgentClient::from_model_config_with_tools(
            model_config,
            provider_config,
            mcp_tools,
            exec_settings,
            pending_approvals,
            pending_write_approvals,
            Some(pending_artifacts.clone()),
            None, // Factory creates session on-demand when execution is enabled
        )
        .await
        .context("Failed to create agent from config")?;

        let now = SystemTime::now();

        Ok(Self {
            id,
            title,
            model_id: model_config.id.clone(),
            agent,
            history: Vec::new(),
            system_traces: Vec::new(),
            attachment_paths: Vec::new(),
            message_timestamps: Vec::new(),
            message_feedback: Vec::new(),
            regeneration_records: Vec::new(),
            token_usage: ConversationTokenUsage::new(),
            created_at: now,
            updated_at: now,
            streaming_message: None,
            pending_artifacts,
            shell_session,
        })
    }

    /// Restore a conversation from persisted data
    #[allow(clippy::too_many_arguments)]
    pub async fn from_data(
        data: ConversationData,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
        exec_settings: Option<crate::settings::models::ExecutionSettingsModel>,
        pending_approvals: Option<
            crate::chatty::models::execution_approval_store::PendingApprovals,
        >,
        pending_write_approvals: Option<
            crate::chatty::models::write_approval_store::PendingWriteApprovals,
        >,
    ) -> Result<Self> {
        // Log URL information
        let url_info = provider_config
            .base_url
            .as_ref()
            .map(|url| format!(" with URL: {}", url))
            .unwrap_or_else(|| " (using default URL)".to_string());
        trace!(
            "Restoring conversation {} with provider: {:?}{}, model: {}",
            data.id, provider_config.provider_type, url_info, model_config.model_identifier
        );

        let pending_artifacts: PendingArtifacts =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        // Reconstruct agent; factory creates shell session on-demand when execution is enabled
        let (agent, shell_session) = AgentClient::from_model_config_with_tools(
            model_config,
            provider_config,
            mcp_tools,
            exec_settings,
            pending_approvals,
            pending_write_approvals,
            Some(pending_artifacts.clone()),
            None, // Factory creates session on-demand
        )
        .await
        .context("Failed to create agent from config")?;

        // Deserialize message history
        let history = Self::deserialize_history(&data.message_history)
            .context("Failed to deserialize message history")?;

        // Deserialize system traces
        let system_traces = Self::deserialize_traces(&data.system_traces)
            .context("Failed to deserialize system traces")?;

        // Deserialize attachment paths
        let attachment_paths =
            Self::deserialize_attachment_paths(&data.attachment_paths).unwrap_or_default();

        // Deserialize message timestamps (with fallback to empty if not present)
        let message_timestamps =
            Self::deserialize_message_timestamps(&data.message_timestamps).unwrap_or_default();

        // Deserialize message feedback (with fallback to empty if not present)
        let message_feedback =
            Self::deserialize_message_feedback(&data.message_feedback).unwrap_or_default();

        // Deserialize regeneration records (with fallback to empty if not present)
        let regeneration_records =
            Self::deserialize_regeneration_records(&data.regeneration_records).unwrap_or_default();

        // Deserialize token usage (with fallback to empty if not present)
        let token_usage = Self::deserialize_token_usage(&data.token_usage)
            .unwrap_or_else(|_| ConversationTokenUsage::new());

        // Convert Unix timestamps to SystemTime
        let created_at = UNIX_EPOCH + Duration::from_secs(data.created_at as u64);
        let updated_at = UNIX_EPOCH + Duration::from_secs(data.updated_at as u64);

        Ok(Self {
            id: data.id,
            title: data.title,
            model_id: data.model_id,
            agent,
            history,
            system_traces,
            attachment_paths,
            message_timestamps,
            message_feedback,
            regeneration_records,
            token_usage,
            created_at,
            updated_at,
            streaming_message: None, // Always start fresh, streaming state is transient
            pending_artifacts,
            shell_session,
        })
    }

    /// Add user message to history with attachment paths
    pub fn add_user_message_with_attachments(
        &mut self,
        message: Message,
        attachments: Vec<PathBuf>,
    ) {
        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        self.history.push(message);
        self.system_traces.push(None);
        self.attachment_paths.push(attachments);
        self.message_timestamps.push(Some(timestamp));
        self.message_feedback.push(None);
        self.updated_at = now;
    }

    /// Finalize response after stream is consumed
    pub fn finalize_response(&mut self, response_text: String) {
        let assistant_message = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: response_text,
            })),
        };

        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        self.history.push(assistant_message);
        self.attachment_paths.push(Vec::new());
        self.message_timestamps.push(Some(timestamp));
        self.message_feedback.push(None);
        self.updated_at = now;
    }

    /// Add a trace for the most recent message
    pub fn add_trace(&mut self, trace: Option<serde_json::Value>) {
        self.system_traces.push(trace);
        self.updated_at = SystemTime::now();
    }

    /// Get conversation ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get conversation title
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Set conversation title
    pub fn set_title(&mut self, title: String) {
        self.title = title;
        self.updated_at = SystemTime::now();
    }

    /// Get model ID
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Get the complete conversation history
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Get system traces
    pub fn system_traces(&self) -> &[Option<serde_json::Value>] {
        &self.system_traces
    }

    /// Get creation timestamp
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// Get last update timestamp
    pub fn updated_at(&self) -> SystemTime {
        self.updated_at
    }

    /// Get the count of messages in history
    pub fn message_count(&self) -> usize {
        self.history.len()
    }

    /// Serialize message history to JSON string
    pub fn serialize_history(&self) -> Result<String> {
        serde_json::to_string(&self.history).context("Failed to serialize message history")
    }

    /// Deserialize message history from JSON string
    pub fn deserialize_history(json: &str) -> Result<Vec<Message>> {
        serde_json::from_str(json).context("Failed to deserialize message history")
    }

    /// Serialize system traces to JSON string
    pub fn serialize_traces(&self) -> Result<String> {
        serde_json::to_string(&self.system_traces).context("Failed to serialize system traces")
    }

    /// Deserialize system traces from JSON string
    pub fn deserialize_traces(json: &str) -> Result<Vec<Option<serde_json::Value>>> {
        serde_json::from_str(json).context("Failed to deserialize system traces")
    }

    /// Get attachment paths (parallel to history)
    pub fn attachment_paths(&self) -> &[Vec<PathBuf>] {
        &self.attachment_paths
    }

    /// Serialize attachment paths to JSON string
    pub fn serialize_attachment_paths(&self) -> Result<String> {
        serde_json::to_string(&self.attachment_paths)
            .context("Failed to serialize attachment paths")
    }

    /// Deserialize attachment paths from JSON string
    pub fn deserialize_attachment_paths(json: &str) -> Result<Vec<Vec<PathBuf>>> {
        serde_json::from_str(json).context("Failed to deserialize attachment paths")
    }

    /// Get message timestamps (parallel to history)
    #[allow(dead_code)]
    pub fn message_timestamps(&self) -> &[Option<i64>] {
        &self.message_timestamps
    }

    /// Serialize message timestamps to JSON string
    pub fn serialize_message_timestamps(&self) -> Result<String> {
        serde_json::to_string(&self.message_timestamps)
            .context("Failed to serialize message timestamps")
    }

    /// Deserialize message timestamps from JSON string
    pub fn deserialize_message_timestamps(json: &str) -> Result<Vec<Option<i64>>> {
        serde_json::from_str(json).context("Failed to deserialize message timestamps")
    }

    /// Get message feedback (parallel to history)
    pub fn message_feedback(&self) -> &[Option<MessageFeedback>] {
        &self.message_feedback
    }

    /// Set feedback for a specific message by index
    pub fn set_message_feedback(&mut self, index: usize, feedback: Option<MessageFeedback>) {
        if index < self.message_feedback.len() {
            self.message_feedback[index] = feedback;
            self.updated_at = SystemTime::now();
        }
    }

    /// Serialize message feedback to JSON string
    pub fn serialize_message_feedback(&self) -> Result<String> {
        serde_json::to_string(&self.message_feedback)
            .context("Failed to serialize message feedback")
    }

    /// Deserialize message feedback from JSON string
    pub fn deserialize_message_feedback(json: &str) -> Result<Vec<Option<MessageFeedback>>> {
        serde_json::from_str(json).context("Failed to deserialize message feedback")
    }

    /// Get regeneration records for this conversation
    #[allow(dead_code)]
    pub fn regeneration_records(&self) -> &[RegenerationRecord] {
        &self.regeneration_records
    }

    /// Record a regeneration event, capturing the original assistant response text
    /// before it is replaced. This creates a DPO preference pair where the original
    /// text is the "rejected" response and the new response (after regeneration) is "chosen".
    #[allow(dead_code)]
    pub fn record_regeneration(
        &mut self,
        message_index: usize,
        original_text: String,
        original_timestamp: i64,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        self.regeneration_records.push(RegenerationRecord {
            message_index,
            original_text,
            original_timestamp,
            regeneration_timestamp: now,
        });
        self.updated_at = SystemTime::now();
    }

    /// Serialize regeneration records to JSON string
    pub fn serialize_regeneration_records(&self) -> Result<String> {
        serde_json::to_string(&self.regeneration_records)
            .context("Failed to serialize regeneration records")
    }

    /// Deserialize regeneration records from JSON string
    pub fn deserialize_regeneration_records(json: &str) -> Result<Vec<RegenerationRecord>> {
        serde_json::from_str(json).context("Failed to deserialize regeneration records")
    }

    /// Get the agent
    pub fn agent(&self) -> &AgentClient {
        &self.agent
    }

    /// Get the pending artifacts handle for this conversation's tools
    pub fn pending_artifacts(&self) -> PendingArtifacts {
        self.pending_artifacts.clone()
    }

    /// Get the shell session for this conversation (if enabled)
    pub fn shell_session(&self) -> Option<std::sync::Arc<ShellSession>> {
        self.shell_session.clone()
    }

    /// Set or replace the shell session for this conversation
    pub fn set_shell_session(&mut self, session: Option<std::sync::Arc<ShellSession>>) {
        self.shell_session = session;
        self.updated_at = SystemTime::now();
    }

    /// Set the agent and model ID synchronously (for model switching without blocking)
    pub fn set_agent(&mut self, agent: AgentClient, model_id: String) {
        self.agent = agent;
        self.model_id = model_id;
        self.updated_at = SystemTime::now();
    }

    /// Get token usage stats
    pub fn token_usage(&self) -> &ConversationTokenUsage {
        &self.token_usage
    }

    /// Add token usage for the most recent exchange
    pub fn add_token_usage(&mut self, usage: TokenUsage) {
        self.token_usage.add_usage(usage);
        self.updated_at = SystemTime::now();
    }

    /// Serialize token usage to JSON string
    pub fn serialize_token_usage(&self) -> Result<String> {
        serde_json::to_string(&self.token_usage).context("Failed to serialize token usage")
    }

    /// Deserialize token usage from JSON string
    pub fn deserialize_token_usage(json: &str) -> Result<ConversationTokenUsage> {
        serde_json::from_str(json).context("Failed to deserialize token usage")
    }

    /// Get the current streaming message content (if any)
    pub fn streaming_message(&self) -> Option<&String> {
        self.streaming_message.as_ref()
    }

    /// Set the streaming message content
    pub fn set_streaming_message(&mut self, content: Option<String>) {
        self.streaming_message = content;
    }

    /// Append text to the streaming message
    pub fn append_streaming_content(&mut self, text: &str) {
        if let Some(ref mut content) = self.streaming_message {
            content.push_str(text);
        } else {
            self.streaming_message = Some(text.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regeneration_record_serialize_roundtrip() {
        let record = RegenerationRecord {
            message_index: 3,
            original_text: "The original response text".to_string(),
            original_timestamp: 1700000000,
            regeneration_timestamp: 1700001000,
        };

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: RegenerationRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(record, deserialized);
    }

    #[test]
    fn test_regeneration_records_vec_serialize_roundtrip() {
        let records = vec![
            RegenerationRecord {
                message_index: 1,
                original_text: "First original".to_string(),
                original_timestamp: 1700000000,
                regeneration_timestamp: 1700001000,
            },
            RegenerationRecord {
                message_index: 1,
                original_text: "Second original (same message re-regenerated)".to_string(),
                original_timestamp: 1700001000,
                regeneration_timestamp: 1700002000,
            },
            RegenerationRecord {
                message_index: 5,
                original_text: "Different message regenerated".to_string(),
                original_timestamp: 1700003000,
                regeneration_timestamp: 1700004000,
            },
        ];

        let json = serde_json::to_string(&records).unwrap();
        let deserialized: Vec<RegenerationRecord> = serde_json::from_str(&json).unwrap();

        assert_eq!(records, deserialized);
    }

    #[test]
    fn test_empty_regeneration_records_deserialize() {
        let json = "[]";
        let records: Vec<RegenerationRecord> = serde_json::from_str(json).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_multiple_regenerations_same_message() {
        let records = vec![
            RegenerationRecord {
                message_index: 3,
                original_text: "Attempt 1".to_string(),
                original_timestamp: 1700000000,
                regeneration_timestamp: 1700001000,
            },
            RegenerationRecord {
                message_index: 3,
                original_text: "Attempt 2".to_string(),
                original_timestamp: 1700001000,
                regeneration_timestamp: 1700002000,
            },
        ];

        let json = serde_json::to_string(&records).unwrap();
        let deserialized: Vec<RegenerationRecord> = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0].message_index, 3);
        assert_eq!(deserialized[1].message_index, 3);
        assert_eq!(deserialized[0].original_text, "Attempt 1");
        assert_eq!(deserialized[1].original_text, "Attempt 2");
    }
}
