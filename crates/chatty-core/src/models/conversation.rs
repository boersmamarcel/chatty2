use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::trace;

use rig::OneOrMany;
use rig::completion::Message;
use rig::completion::message::{AssistantContent, Text};

use crate::factories::AgentClient;
use crate::models::message_types::SystemTrace;
use crate::models::token_usage::{ConversationTokenUsage, TokenUsage};
use crate::repositories::ConversationData;
use crate::services::memory_service::MemoryService;
use crate::services::shell_service::ShellSession;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;
use crate::tools::{LocalModuleAgentSummary, PendingArtifacts};

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
    // Both `add_user_message_with_attachments` and `finalize_response` push
    // to all five arrays atomically.
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
    /// Partial streaming trace being composed (None if no active stream)
    streaming_trace: Option<SystemTrace>,
    /// Shared state for artifacts queued by AddAttachmentTool during a stream
    pending_artifacts: PendingArtifacts,
    /// Persistent shell session for this conversation (lazily initialized)
    shell_session: Option<std::sync::Arc<ShellSession>>,
    /// Per-conversation working directory override (overrides the global workspace_dir setting)
    working_dir: Option<PathBuf>,
    /// Effective workspace directory the current agent was built with.
    agent_workspace_dir: Option<PathBuf>,
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
        pending_approvals: Option<crate::models::execution_approval_store::PendingApprovals>,
        pending_write_approvals: Option<crate::models::write_approval_store::PendingWriteApprovals>,
        user_secrets: Vec<(String, String)>,
        theme_colors: Option<[String; 5]>,
        memory_service: Option<MemoryService>,
        search_settings: Option<crate::settings::models::search_settings::SearchSettingsModel>,
        embedding_service: Option<crate::services::embedding_service::EmbeddingService>,
        allow_sub_agent: bool,
        module_agents: Vec<LocalModuleAgentSummary>,
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
        let agent_workspace_dir = exec_settings
            .as_ref()
            .and_then(|settings| settings.workspace_dir.as_ref())
            .map(PathBuf::from);

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
            user_secrets,
            theme_colors,
            memory_service,
            search_settings,
            embedding_service,
            allow_sub_agent,
            module_agents,
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
            streaming_trace: None,
            pending_artifacts,
            shell_session,
            working_dir: None,
            agent_workspace_dir,
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
        pending_approvals: Option<crate::models::execution_approval_store::PendingApprovals>,
        pending_write_approvals: Option<crate::models::write_approval_store::PendingWriteApprovals>,
        user_secrets: Vec<(String, String)>,
        theme_colors: Option<[String; 5]>,
        memory_service: Option<MemoryService>,
        search_settings: Option<crate::settings::models::search_settings::SearchSettingsModel>,
        embedding_service: Option<crate::services::embedding_service::EmbeddingService>,
        allow_sub_agent: bool,
        module_agents: Vec<LocalModuleAgentSummary>,
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
        let agent_workspace_dir = exec_settings
            .as_ref()
            .and_then(|settings| settings.workspace_dir.as_ref())
            .map(PathBuf::from);

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
            user_secrets,
            theme_colors,
            memory_service,
            search_settings,
            embedding_service,
            allow_sub_agent,
            module_agents,
        )
        .await
        .context("Failed to create agent from config")?;

        // Deserialize message history
        let history = Self::deserialize_history(&data.message_history)
            .context("Failed to deserialize message history")?;

        // Deserialize system traces
        let system_traces = Self::deserialize_traces(&data.system_traces)
            .context("Failed to deserialize system traces")?;

        let non_null_traces = system_traces.iter().filter(|t| t.is_some()).count();
        tracing::debug!(
            conv_id = %data.id,
            total_traces = system_traces.len(),
            non_null_traces,
            history_len = history.len(),
            "Deserialized traces in from_data"
        );

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
            streaming_trace: None,
            pending_artifacts,
            shell_session,
            working_dir: data.working_dir.map(PathBuf::from),
            agent_workspace_dir,
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
        self.debug_assert_parallel_arrays_aligned();
    }

    /// Remove the last message from history if it is a User message.
    ///
    /// Used when a stream is cancelled before any assistant content was received:
    /// the user message that triggered the cancelled stream must be rolled back to
    /// avoid leaving a trailing user message with no assistant response, which would
    /// cause LLM API errors on the next request.
    ///
    /// Returns `true` if a user message was removed, `false` otherwise.
    pub fn remove_last_user_message(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let last_idx = self.history.len() - 1;
        if !matches!(self.history[last_idx], Message::User { .. }) {
            return false;
        }

        self.history.pop();
        self.system_traces.pop();
        self.attachment_paths.pop();
        self.message_timestamps.pop();
        self.message_feedback.pop();
        self.updated_at = SystemTime::now();
        self.debug_assert_parallel_arrays_aligned();
        true
    }

    /// Finalize response after stream is consumed.
    /// `attachments` contains paths to files generated by tool calls (e.g. plots)
    /// that should be displayed inline in the assistant message.
    /// `trace` is the system trace (tool calls, thinking blocks) for this response.
    pub fn finalize_response(
        &mut self,
        response_text: String,
        attachments: Vec<PathBuf>,
        trace: Option<serde_json::Value>,
    ) {
        let assistant_message = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: response_text,
            })),
        };

        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        self.history.push(assistant_message);
        self.system_traces.push(trace);
        self.attachment_paths.push(attachments);
        self.message_timestamps.push(Some(timestamp));
        self.message_feedback.push(None);
        self.updated_at = now;
        self.debug_assert_parallel_arrays_aligned();
    }

    /// Verify all parallel arrays have the same length.
    /// Only active in debug builds (zero cost in release).
    fn debug_assert_parallel_arrays_aligned(&self) {
        let len = self.history.len();
        debug_assert_eq!(self.system_traces.len(), len, "system_traces misaligned");
        debug_assert_eq!(
            self.attachment_paths.len(),
            len,
            "attachment_paths misaligned"
        );
        debug_assert_eq!(
            self.message_timestamps.len(),
            len,
            "message_timestamps misaligned"
        );
        debug_assert_eq!(
            self.message_feedback.len(),
            len,
            "message_feedback misaligned"
        );
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
        if self.system_traces.len() != self.history.len() {
            tracing::warn!(
                traces_len = self.system_traces.len(),
                history_len = self.history.len(),
                "system_traces length does not match history length during serialization"
            );
        }
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

    /// Remove the last assistant message and its trace from all parallel arrays.
    /// Returns the (text, timestamp) of the removed message if found, or None.
    /// Used during regeneration to replace the old response.
    pub fn remove_last_assistant_message(&mut self) -> Option<(String, Option<i64>)> {
        if self.history.len() < 2 {
            return None;
        }
        let last_idx = self.history.len() - 1;
        if !matches!(self.history[last_idx], Message::Assistant { .. }) {
            return None;
        }

        // Extract text from assistant message before removing
        let text = match &self.history[last_idx] {
            Message::Assistant { content, .. } => content
                .iter()
                .filter_map(|ac| match ac {
                    AssistantContent::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        let timestamp = self.message_timestamps.get(last_idx).copied().flatten();

        // Pop from all parallel arrays
        self.history.pop();
        self.system_traces.pop();
        self.attachment_paths.pop();
        self.message_timestamps.pop();
        self.message_feedback.pop();

        self.updated_at = SystemTime::now();
        Some((text, timestamp))
    }

    /// Replace the conversation history with a summarized version.
    ///
    /// `new_history` is the output of `summarize_oldest_half()`: a single summary
    /// message followed by the tail of the original history starting at
    /// `original_tail_offset`. All parallel arrays are rebuilt to match:
    /// - Index 0 (summary message) gets default/empty metadata.
    /// - Indices 1..N map to the original entries at `original_tail_offset..`.
    pub fn replace_history(&mut self, new_history: Vec<Message>, original_tail_offset: usize) {
        let tail_start = original_tail_offset.min(self.system_traces.len());

        let mut new_traces = Vec::with_capacity(new_history.len());
        let mut new_attachments = Vec::with_capacity(new_history.len());
        let mut new_timestamps = Vec::with_capacity(new_history.len());
        let mut new_feedback = Vec::with_capacity(new_history.len());

        // Default metadata for the summary message at index 0
        new_traces.push(None);
        new_attachments.push(vec![]);
        new_timestamps.push(None);
        new_feedback.push(None);

        // Preserve metadata for the kept tail of the original history
        new_traces.extend(self.system_traces[tail_start..].iter().cloned());
        new_attachments.extend(self.attachment_paths[tail_start..].iter().cloned());
        new_timestamps.extend(self.message_timestamps[tail_start..].iter().cloned());
        new_feedback.extend(self.message_feedback[tail_start..].iter().cloned());

        self.history = new_history;
        self.system_traces = new_traces;
        self.attachment_paths = new_attachments;
        self.message_timestamps = new_timestamps;
        self.message_feedback = new_feedback;
        self.updated_at = SystemTime::now();
        self.debug_assert_parallel_arrays_aligned();
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

    /// Get the per-conversation working directory override
    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.working_dir.as_ref()
    }

    /// Get the effective workspace directory the current agent was built with
    pub fn agent_workspace_dir(&self) -> Option<&PathBuf> {
        self.agent_workspace_dir.as_ref()
    }

    /// Set or clear the per-conversation working directory override
    pub fn set_working_dir(&mut self, dir: Option<PathBuf>) {
        self.working_dir = dir;
        self.updated_at = SystemTime::now();
    }

    /// Set the agent and model ID synchronously (for model switching without blocking)
    pub fn set_agent(
        &mut self,
        agent: AgentClient,
        model_id: String,
        agent_workspace_dir: Option<PathBuf>,
    ) {
        self.agent = agent;
        self.model_id = model_id;
        self.agent_workspace_dir = agent_workspace_dir;
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

    /// Get the current streaming trace (if any)
    pub fn streaming_trace(&self) -> Option<&SystemTrace> {
        self.streaming_trace.as_ref()
    }

    /// Get a mutable reference to the current streaming trace (if any)
    pub fn streaming_trace_mut(&mut self) -> Option<&mut SystemTrace> {
        self.streaming_trace.as_mut()
    }

    /// Set the streaming trace
    pub fn set_streaming_trace(&mut self, trace: Option<SystemTrace>) {
        self.streaming_trace = trace;
    }

    /// Get or create the streaming trace, returning a mutable reference
    pub fn ensure_streaming_trace(&mut self) -> &mut SystemTrace {
        self.streaming_trace.get_or_insert_with(SystemTrace::new)
    }

    /// Append text to the streaming message
    pub fn append_streaming_content(&mut self, text: &str) {
        if let Some(ref mut content) = self.streaming_message {
            content.push_str(text);
        } else {
            let mut s = String::with_capacity(4096);
            s.push_str(text);
            self.streaming_message = Some(s);
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
