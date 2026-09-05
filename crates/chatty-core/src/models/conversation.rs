use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::trace;

use rig_core::completion::Message;
use rig_core::completion::message::{AssistantContent, Text};

use crate::factories::AgentClient;
use crate::factories::agent_factory::AgentBuildContext;
use crate::models::message_types::{SystemTrace, ToolSource, TraceItem};
use crate::models::token_usage::{ConversationTokenUsage, TokenUsage};
use crate::repositories::ConversationData;
use crate::services::AgentTaskSnapshot;
use crate::services::is_tool_result_message;
use crate::services::shell_service::ShellSession;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;
use crate::tools::PendingArtifacts;

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

/// Per-message metadata stored alongside the rig `Message`.
///
/// This replaces the previous parallel-arrays design where separate Vecs for
/// traces, attachments, timestamps, and feedback had to be kept manually
/// synchronized. With `MessageEntry`, all per-message data is collocated and
/// impossible to desynchronize.
#[derive(Clone, Debug)]
pub struct MessageEntry {
    pub message: Message,
    pub system_trace: Option<serde_json::Value>,
    pub attachment_paths: Vec<PathBuf>,
    pub timestamp: Option<i64>,
    pub feedback: Option<MessageFeedback>,
}

/// A single conversation with an AI agent
pub struct Conversation {
    id: String,
    title: String,
    model_id: String,
    agent: AgentClient,
    /// All messages with their metadata, in chronological order.
    entries: Vec<MessageEntry>,
    /// Regeneration records capturing original responses before replacement (DPO preference pairs)
    regeneration_records: Vec<RegenerationRecord>,
    token_usage: ConversationTokenUsage,
    created_at: SystemTime,
    updated_at: SystemTime,
    /// Partial streaming message being composed (None if no active stream)
    streaming_message: Option<String>,
    /// Partial streaming trace being composed (None if no active stream)
    streaming_trace: Option<SystemTrace>,
    /// Transient sub-agent progress trace shown as a separate in-progress UI message.
    /// This is kept in-memory so switching conversations during an active
    /// sub-agent run can restore the trace and its source badge.
    streaming_sub_agent_trace: Option<SystemTrace>,
    /// rig's record of the turn in flight — its tool-call and tool-result
    /// messages are persisted behind the final text (AGE-247). Set from the
    /// stream's `TurnMessages` chunk, consumed by `finalize_response`.
    streaming_turn_messages: Option<Vec<Message>>,
    /// Shared state for artifacts queued by AddAttachmentTool during a stream
    pending_artifacts: PendingArtifacts,
    /// Persistent shell session for this conversation (lazily initialized)
    shell_session: Option<std::sync::Arc<ShellSession>>,
    /// Per-conversation working directory override (overrides the global workspace_dir setting)
    working_dir: Option<PathBuf>,
    /// Latest persisted agent todo panel snapshot for this conversation.
    agent_task_snapshot: Option<AgentTaskSnapshot>,
    /// Effective workspace directory the current agent was built with.
    agent_workspace_dir: Option<PathBuf>,
    /// Progress slot for the invoke_agent tool in this conversation's agent.
    invoke_agent_progress_slot: crate::tools::invoke_agent_tool::InvokeAgentProgressSlot,
}

impl Conversation {
    /// Create a new conversation from model and provider config
    pub async fn new(
        id: String,
        title: String,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        ctx: AgentBuildContext,
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
        let agent_workspace_dir = ctx
            .exec_settings
            .as_ref()
            .and_then(|settings| settings.workspace_dir.as_ref())
            .map(PathBuf::from);

        // Shell session is created inside the factory when execution is enabled.
        // The factory returns it so we can store it on the Conversation for reuse
        // across agent rebuilds (MCP changes, model switches).
        let ctx = AgentBuildContext {
            pending_artifacts: Some(pending_artifacts.clone()),
            shell_session: None, // Factory creates session on-demand when execution is enabled
            ..ctx
        };
        let (agent, shell_session, invoke_agent_progress_slot) =
            AgentClient::from_model_config_with_tools(model_config, provider_config, ctx)
                .await
                .context("Failed to create agent from config")?;

        let now = SystemTime::now();

        Ok(Self {
            id,
            title,
            model_id: model_config.id.clone(),
            agent,
            entries: Vec::new(),
            regeneration_records: Vec::new(),
            token_usage: ConversationTokenUsage::new(),
            created_at: now,
            updated_at: now,
            streaming_message: None,
            streaming_trace: None,
            streaming_sub_agent_trace: None,
            streaming_turn_messages: None,
            pending_artifacts,
            shell_session,
            working_dir: None,
            agent_task_snapshot: None,
            agent_workspace_dir,
            invoke_agent_progress_slot,
        })
    }

    /// Restore a conversation from persisted data
    pub async fn from_data(
        data: ConversationData,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        ctx: AgentBuildContext,
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
        let agent_workspace_dir = ctx
            .exec_settings
            .as_ref()
            .and_then(|settings| settings.workspace_dir.as_ref())
            .map(PathBuf::from);

        // Reconstruct agent; factory creates shell session on-demand when execution is enabled
        let ctx = AgentBuildContext {
            pending_artifacts: Some(pending_artifacts.clone()),
            shell_session: None, // Factory creates session on-demand
            ..ctx
        };
        let (agent, shell_session, invoke_agent_progress_slot) =
            AgentClient::from_model_config_with_tools(model_config, provider_config, ctx)
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

        // Deserialize per-message metadata (with fallbacks for older data)
        let attachment_paths =
            Self::deserialize_attachment_paths(&data.attachment_paths).unwrap_or_default();
        let message_timestamps =
            Self::deserialize_message_timestamps(&data.message_timestamps).unwrap_or_default();
        let message_feedback =
            Self::deserialize_message_feedback(&data.message_feedback).unwrap_or_default();

        // Zip the deserialized arrays into MessageEntry structs
        let entries: Vec<MessageEntry> = history
            .into_iter()
            .enumerate()
            .map(|(i, message)| MessageEntry {
                message,
                system_trace: system_traces.get(i).cloned().flatten(),
                attachment_paths: attachment_paths.get(i).cloned().unwrap_or_default(),
                timestamp: message_timestamps.get(i).copied().flatten(),
                feedback: message_feedback.get(i).cloned().flatten(),
            })
            .collect();

        // Deserialize regeneration records (with fallback to empty if not present)
        let regeneration_records =
            Self::deserialize_regeneration_records(&data.regeneration_records).unwrap_or_default();

        // Deserialize token usage (with fallback to empty if not present)
        let token_usage = Self::deserialize_token_usage(&data.token_usage)
            .unwrap_or_else(|_| ConversationTokenUsage::new());
        let agent_task_snapshot = data
            .agent_task_snapshot
            .as_deref()
            .and_then(|json| Self::deserialize_agent_task_snapshot(json).ok());

        // Convert Unix timestamps to SystemTime
        let created_at = UNIX_EPOCH + Duration::from_secs(data.created_at as u64);
        let updated_at = UNIX_EPOCH + Duration::from_secs(data.updated_at as u64);

        Ok(Self {
            id: data.id,
            title: data.title,
            model_id: data.model_id,
            agent,
            entries,
            regeneration_records,
            token_usage,
            created_at,
            updated_at,
            streaming_message: None, // Always start fresh, streaming state is transient
            streaming_trace: None,
            streaming_sub_agent_trace: None,
            streaming_turn_messages: None,
            pending_artifacts,
            shell_session,
            working_dir: data.working_dir.map(PathBuf::from),
            agent_task_snapshot,
            agent_workspace_dir,
            invoke_agent_progress_slot,
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
        self.entries.push(MessageEntry {
            message,
            system_trace: None,
            attachment_paths: attachments,
            timestamp: Some(timestamp),
            feedback: None,
        });
        self.updated_at = now;
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
        if let Some(last) = self.entries.last()
            && is_user_text_message(&last.message)
        {
            self.entries.pop();
            self.updated_at = SystemTime::now();
            return true;
        }
        false
    }

    /// Finalize response after stream is consumed.
    /// `attachments` contains paths to files generated by tool calls (e.g. plots)
    /// that should be displayed inline in the assistant message.
    /// `trace` is the system trace (tool calls, thinking blocks) for this response.
    ///
    /// When the stream delivered rig's record of the turn
    /// (`set_streaming_turn_messages`), its tool-call and tool-result messages
    /// are persisted first, in order, so the model sees its own tool activity
    /// on later turns (AGE-247). They carry no trace and no attachments; both
    /// stay on the final text entry, which the transcript renders from.
    pub fn finalize_response(
        &mut self,
        response_text: String,
        attachments: Vec<PathBuf>,
        trace: Option<serde_json::Value>,
    ) {
        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        finalize_response_state(
            &mut self.entries,
            self.streaming_turn_messages.take(),
            response_text,
            attachments,
            trace,
            timestamp,
        );
        self.updated_at = now;
    }

    /// Keep rig's record of the turn in flight until `finalize_response`.
    pub fn set_streaming_turn_messages(&mut self, messages: Option<Vec<Message>>) {
        self.streaming_turn_messages = messages;
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

    /// Get all message entries (messages with their metadata)
    pub fn entries(&self) -> &[MessageEntry] {
        &self.entries
    }

    /// Collect just the rig `Message`s (for LLM API calls).
    /// Callers that need messages alongside their metadata should use `entries()`.
    pub fn messages(&self) -> Vec<Message> {
        self.entries.iter().map(|e| e.message.clone()).collect()
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
        self.entries.len()
    }

    // ── Serialization (backward-compatible with ConversationData format) ─────

    /// Serialize message history to JSON string
    pub fn serialize_history(&self) -> Result<String> {
        let messages: Vec<&Message> = self.entries.iter().map(|e| &e.message).collect();
        serde_json::to_string(&messages).context("Failed to serialize message history")
    }

    /// Deserialize message history from JSON string
    pub fn deserialize_history(json: &str) -> Result<Vec<Message>> {
        serde_json::from_str(json).context("Failed to deserialize message history")
    }

    /// Serialize system traces to JSON string
    pub fn serialize_traces(&self) -> Result<String> {
        let traces: Vec<Option<&serde_json::Value>> = self
            .entries
            .iter()
            .map(|e| e.system_trace.as_ref())
            .collect();
        serde_json::to_string(&traces).context("Failed to serialize system traces")
    }

    /// Deserialize system traces from JSON string
    pub fn deserialize_traces(json: &str) -> Result<Vec<Option<serde_json::Value>>> {
        serde_json::from_str(json).context("Failed to deserialize system traces")
    }

    /// Serialize attachment paths to JSON string
    pub fn serialize_attachment_paths(&self) -> Result<String> {
        let paths: Vec<&Vec<PathBuf>> = self.entries.iter().map(|e| &e.attachment_paths).collect();
        serde_json::to_string(&paths).context("Failed to serialize attachment paths")
    }

    /// Deserialize attachment paths from JSON string
    pub fn deserialize_attachment_paths(json: &str) -> Result<Vec<Vec<PathBuf>>> {
        serde_json::from_str(json).context("Failed to deserialize attachment paths")
    }

    /// Serialize message timestamps to JSON string
    pub fn serialize_message_timestamps(&self) -> Result<String> {
        let timestamps: Vec<Option<i64>> = self.entries.iter().map(|e| e.timestamp).collect();
        serde_json::to_string(&timestamps).context("Failed to serialize message timestamps")
    }

    /// Deserialize message timestamps from JSON string
    pub fn deserialize_message_timestamps(json: &str) -> Result<Vec<Option<i64>>> {
        serde_json::from_str(json).context("Failed to deserialize message timestamps")
    }

    /// Set feedback for a specific message by index
    pub fn set_message_feedback(&mut self, index: usize, feedback: Option<MessageFeedback>) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.feedback = feedback;
            self.updated_at = SystemTime::now();
        }
    }

    /// Append a trace item to the activity trail of the most recent
    /// assistant message, once that message has settled (AGE-156: browser
    /// control handoffs happen between turns, after the streaming trace is
    /// gone). Returns `false` when there is no assistant message to attach
    /// it to.
    pub fn append_trace_item_to_last_assistant(&mut self, item: TraceItem) -> bool {
        let appended = append_trace_item_state(&mut self.entries, item);
        if appended {
            self.updated_at = SystemTime::now();
        }
        appended
    }

    /// Serialize message feedback to JSON string
    pub fn serialize_message_feedback(&self) -> Result<String> {
        let feedback: Vec<Option<&MessageFeedback>> =
            self.entries.iter().map(|e| e.feedback.as_ref()).collect();
        serde_json::to_string(&feedback).context("Failed to serialize message feedback")
    }

    /// Deserialize message feedback from JSON string
    pub fn deserialize_message_feedback(json: &str) -> Result<Vec<Option<MessageFeedback>>> {
        serde_json::from_str(json).context("Failed to deserialize message feedback")
    }

    /// Serialize the persisted agent task snapshot to JSON.
    pub fn serialize_agent_task_snapshot(&self) -> Result<Option<String>> {
        self.agent_task_snapshot
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("Failed to serialize agent task snapshot")
    }

    /// Deserialize the persisted agent task snapshot from JSON.
    pub fn deserialize_agent_task_snapshot(json: &str) -> Result<AgentTaskSnapshot> {
        serde_json::from_str(json).context("Failed to deserialize agent task snapshot")
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

    /// Remove the last assistant message and its metadata, together with the
    /// tool round-trips of its turn (AGE-247), back to the user text message
    /// that started it.
    /// Returns the (text, timestamp) of the removed message if found, or None.
    /// Used during regeneration to replace the old response.
    pub fn remove_last_assistant_message(&mut self) -> Option<(String, Option<i64>)> {
        let removed = remove_last_turn_state(&mut self.entries)?;
        self.updated_at = SystemTime::now();
        Some(removed)
    }

    /// Replace the conversation history with a summarized version.
    ///
    /// `new_history` is the output of `summarize_oldest_half()`: a single summary
    /// message followed by the tail of the original history starting at
    /// `original_tail_offset`. Metadata from the preserved tail is carried over;
    /// the summary message at index 0 gets default/empty metadata.
    pub fn replace_history(&mut self, new_history: Vec<Message>, original_tail_offset: usize) {
        let tail_start = original_tail_offset.min(self.entries.len());

        let mut new_entries = Vec::with_capacity(new_history.len());

        // Summary message at index 0 with default metadata
        if let Some(summary_msg) = new_history.first() {
            new_entries.push(MessageEntry {
                message: summary_msg.clone(),
                system_trace: None,
                attachment_paths: vec![],
                timestamp: None,
                feedback: None,
            });
        }

        // Preserve metadata from the kept tail of the original history
        for (msg, old_entry) in new_history
            .into_iter()
            .skip(1)
            .zip(self.entries[tail_start..].iter())
        {
            new_entries.push(MessageEntry {
                message: msg,
                system_trace: old_entry.system_trace.clone(),
                attachment_paths: old_entry.attachment_paths.clone(),
                timestamp: old_entry.timestamp,
                feedback: old_entry.feedback.clone(),
            });
        }

        self.entries = new_entries;
        self.updated_at = SystemTime::now();
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

    /// Get the invoke_agent progress slot for this conversation's agent
    pub fn invoke_agent_progress_slot(
        &self,
    ) -> crate::tools::invoke_agent_tool::InvokeAgentProgressSlot {
        self.invoke_agent_progress_slot.clone()
    }

    /// Set the invoke_agent progress slot (after agent rebuild)
    pub fn set_invoke_agent_progress_slot(
        &mut self,
        slot: crate::tools::invoke_agent_tool::InvokeAgentProgressSlot,
    ) {
        self.invoke_agent_progress_slot = slot;
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

    /// Get the latest persisted agent todo panel snapshot for this conversation.
    pub fn agent_task_snapshot(&self) -> Option<&AgentTaskSnapshot> {
        self.agent_task_snapshot.as_ref()
    }

    /// Set or clear the persisted agent todo panel snapshot for this conversation.
    pub fn set_agent_task_snapshot(&mut self, snapshot: Option<AgentTaskSnapshot>) {
        self.agent_task_snapshot = snapshot;
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

    pub fn streaming_sub_agent_trace(&self) -> Option<&SystemTrace> {
        self.streaming_sub_agent_trace.as_ref()
    }

    pub fn set_streaming_sub_agent_trace(&mut self, trace: Option<SystemTrace>) {
        self.streaming_sub_agent_trace = trace;
    }

    pub fn start_sub_agent_progress(&mut self, prompt: &str, source: ToolSource) {
        start_sub_agent_progress_state(&mut self.streaming_sub_agent_trace, prompt, source);
    }

    pub fn append_sub_agent_progress(&mut self, line: &str) {
        append_sub_agent_progress_state(&mut self.streaming_sub_agent_trace, line);
    }

    pub fn finalize_sub_agent_progress(&mut self, success: bool, result: Option<String>) {
        finalize_sub_agent_progress_state(&mut self.streaming_sub_agent_trace, success, result);
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
            let mut s = String::with_capacity(32_768);
            s.push_str(text);
            self.streaming_message = Some(s);
        }
    }
}

/// A user message that a human (or a summary) wrote, as opposed to tool results.
fn is_user_text_message(message: &Message) -> bool {
    matches!(message, Message::User { .. }) && !is_tool_result_message(message)
}

/// The tool round-trips out of rig's record of a turn.
///
/// rig's list is the prompt, then alternating assistant tool-call and user
/// tool-result messages, then the final assistant text. The prompt is already
/// persisted and the final text is persisted from the streamed response, so
/// only what lies between them is taken. The cut is made after the last tool
/// result: an assistant tool-call message with no result behind it (a run cut
/// off at its turn limit) would be an orphan the OpenAI-compatible endpoints
/// reject, and the text entry that follows closes the turn either way.
fn turn_tool_messages(messages: Vec<Message>) -> Vec<Message> {
    let start = messages
        .iter()
        .position(|m| matches!(m, Message::Assistant { .. }))
        .unwrap_or(messages.len());
    let end = messages
        .iter()
        .rposition(is_tool_result_message)
        .map(|i| i + 1)
        .unwrap_or(start)
        .max(start);
    messages.into_iter().take(end).skip(start).collect()
}

/// Persist a finished turn: its tool round-trips (when rig's record of the
/// turn is available), then the final text entry carrying trace and
/// attachments. See [`Conversation::finalize_response`].
fn finalize_response_state(
    entries: &mut Vec<MessageEntry>,
    turn_messages: Option<Vec<Message>>,
    response_text: String,
    attachments: Vec<PathBuf>,
    trace: Option<serde_json::Value>,
    timestamp: i64,
) {
    let tool_messages = turn_messages.map(turn_tool_messages).unwrap_or_default();
    entries.extend(tool_messages.into_iter().map(|message| MessageEntry {
        message,
        system_trace: None,
        attachment_paths: Vec::new(),
        timestamp: Some(timestamp),
        feedback: None,
    }));

    entries.push(MessageEntry {
        message: Message::Assistant {
            id: None,
            content: vec![AssistantContent::Text(Text::new(response_text))],
        },
        system_trace: trace,
        attachment_paths: attachments,
        timestamp: Some(timestamp),
        feedback: None,
    });
}

/// Drop the last turn — the final assistant entry and the tool round-trips
/// before it, back to the user text message that started it — returning the
/// removed answer's (text, timestamp). See
/// [`Conversation::remove_last_assistant_message`].
fn remove_last_turn_state(entries: &mut Vec<MessageEntry>) -> Option<(String, Option<i64>)> {
    if entries.len() < 2 {
        return None;
    }
    let last = entries.last()?;
    let Message::Assistant { content, .. } = &last.message else {
        return None;
    };
    let text = content
        .iter()
        .filter_map(|ac| match ac {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let timestamp = last.timestamp;

    let turn_start = entries
        .iter()
        .rposition(|e| is_user_text_message(&e.message))
        .map(|i| i + 1)
        .unwrap_or(entries.len() - 1);
    entries.truncate(turn_start);
    Some((text, timestamp))
}

/// Push `item` onto the persisted trace of the last assistant entry,
/// creating the trace when the message had none.
fn append_trace_item_state(entries: &mut [MessageEntry], item: TraceItem) -> bool {
    let Some(entry) = entries
        .iter_mut()
        .rev()
        .find(|e| matches!(e.message, Message::Assistant { .. }))
    else {
        return false;
    };
    // Never `take()` the stored trace first: a trace that fails to parse
    // or re-serialize must stay exactly as it was rather than be replaced
    // by one holding only the new item.
    let mut trace = match entry.system_trace.as_ref() {
        Some(value) => match serde_json::from_value::<SystemTrace>(value.clone()) {
            Ok(trace) => trace,
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to parse stored trace; not appending item");
                return false;
            }
        },
        None => SystemTrace::new(),
    };
    trace.items.push(item);
    match serde_json::to_value(&trace) {
        Ok(value) => {
            entry.system_trace = Some(value);
            true
        }
        Err(e) => {
            tracing::warn!(error = ?e, "Failed to serialize trace after appending item");
            false
        }
    }
}

fn start_sub_agent_progress_state(
    streaming_sub_agent_trace: &mut Option<SystemTrace>,
    prompt: &str,
    source: ToolSource,
) {
    *streaming_sub_agent_trace = Some(SystemTrace::new_sub_agent(prompt, source));
}

fn append_sub_agent_progress_state(
    streaming_sub_agent_trace: &mut Option<SystemTrace>,
    line: &str,
) {
    if let Some(trace) = streaming_sub_agent_trace.as_mut() {
        trace.append_sub_agent_progress(line);
    }
}

fn finalize_sub_agent_progress_state(
    streaming_sub_agent_trace: &mut Option<SystemTrace>,
    success: bool,
    result: Option<String>,
) {
    if let Some(trace) = streaming_sub_agent_trace.as_mut() {
        trace.finalize_sub_agent_progress(success, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::message_types::TraceItem;

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

    #[test]
    fn deserialize_traces_preserves_running_sub_agent_source() {
        let mut trace = SystemTrace::new_sub_agent("review this thread", ToolSource::HiveCloud);
        trace.append_sub_agent_progress("Working...");

        let json =
            serde_json::to_string(&vec![Some(serde_json::to_value(&trace).unwrap())]).unwrap();
        let traces = Conversation::deserialize_traces(&json).unwrap();
        let restored_trace = traces[0].clone().unwrap();
        let restored: SystemTrace = serde_json::from_value(restored_trace).unwrap();

        let tc = match &restored.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };

        assert_eq!(tc.source, ToolSource::HiveCloud);
        assert!(restored.is_running_sub_agent());
        assert_eq!(tc.output.as_deref(), Some("Working..."));
    }

    #[test]
    fn sub_agent_progress_does_not_touch_parent_body() {
        let streaming_message = Some("assistant body".to_string());
        let streaming_trace = Some(SystemTrace::new());
        let mut streaming_sub_agent_trace = None;

        start_sub_agent_progress_state(
            &mut streaming_sub_agent_trace,
            "investigate",
            ToolSource::Local,
        );
        append_sub_agent_progress_state(&mut streaming_sub_agent_trace, "working...");
        finalize_sub_agent_progress_state(
            &mut streaming_sub_agent_trace,
            true,
            Some("done".to_string()),
        );

        assert_eq!(streaming_message.as_deref(), Some("assistant body"));
        assert!(
            streaming_trace
                .as_ref()
                .is_some_and(|trace| trace.items.is_empty()),
            "parent streaming_trace must not be replaced by the sub-agent progress card"
        );

        let trace = streaming_sub_agent_trace
            .as_ref()
            .expect("sub-agent trace should persist");
        let tc = match &trace.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };
        assert_eq!(tc.output.as_deref(), Some("working...\n\n---\n\ndone"));
        assert!(!trace.is_running_sub_agent());
    }

    fn entry(message: Message, system_trace: Option<serde_json::Value>) -> MessageEntry {
        MessageEntry {
            message,
            system_trace,
            attachment_paths: Vec::new(),
            timestamp: None,
            feedback: None,
        }
    }

    fn handoff_item() -> TraceItem {
        TraceItem::ToolCall(
            crate::models::message_types::ToolCallBlock::browser_control_handoff(
                true,
                "https://example.com",
            ),
        )
    }

    #[test]
    fn append_trace_item_targets_the_last_assistant_entry() {
        let existing =
            serde_json::to_value(SystemTrace::new_sub_agent("x", ToolSource::Local)).unwrap();
        let mut entries = vec![
            entry(Message::user("hi"), None),
            entry(Message::assistant("first"), Some(existing)),
            entry(Message::user("again"), None),
            entry(Message::assistant("second"), None),
        ];

        assert!(append_trace_item_state(&mut entries, handoff_item()));

        // The earlier assistant message is untouched.
        let first: SystemTrace =
            serde_json::from_value(entries[1].system_trace.clone().unwrap()).unwrap();
        assert_eq!(first.items.len(), 1);
        // The last one gained a fresh trace holding only the handoff.
        let last: SystemTrace =
            serde_json::from_value(entries[3].system_trace.clone().unwrap()).unwrap();
        assert_eq!(last.items.len(), 1);
        match &last.items[0] {
            TraceItem::ToolCall(tc) => assert_eq!(tc.tool_name, "browser_take_control"),
            other => panic!("expected ToolCall, got {other:?}"),
        }
        assert!(entries[2].system_trace.is_none());
    }

    #[test]
    fn append_trace_item_extends_an_existing_trace() {
        let existing =
            serde_json::to_value(SystemTrace::new_sub_agent("x", ToolSource::Local)).unwrap();
        let mut entries = vec![entry(Message::assistant("a"), Some(existing))];

        assert!(append_trace_item_state(&mut entries, handoff_item()));

        let trace: SystemTrace =
            serde_json::from_value(entries[0].system_trace.clone().unwrap()).unwrap();
        assert_eq!(trace.items.len(), 2);
    }

    #[test]
    fn append_trace_item_leaves_an_unparseable_trace_untouched() {
        let garbage = serde_json::json!("not a trace");
        let mut entries = vec![entry(Message::assistant("a"), Some(garbage.clone()))];

        assert!(!append_trace_item_state(&mut entries, handoff_item()));
        assert_eq!(entries[0].system_trace, Some(garbage));
    }

    #[test]
    fn append_trace_item_without_an_assistant_message_is_a_no_op() {
        let mut entries = vec![entry(Message::user("hi"), None)];
        assert!(!append_trace_item_state(&mut entries, handoff_item()));
        assert!(entries[0].system_trace.is_none());
    }

    // -------------------------------------------------------------------
    // Tool turns persisted with the answer (AGE-247)
    // -------------------------------------------------------------------

    fn tool_call(id: &str, text: Option<&str>) -> Message {
        let mut content = Vec::new();
        if let Some(text) = text {
            content.push(AssistantContent::text(text));
        }
        content.push(AssistantContent::tool_call(
            id,
            "read_file",
            serde_json::json!({ "path": format!("{id}.md") }),
        ));
        Message::Assistant { id: None, content }
    }

    fn tool_result(id: &str) -> Message {
        Message::tool_result(id, "read_file", format!("contents of {id}"))
    }

    /// rig's record of a turn with two tool calls: the prompt, two
    /// call/result pairs, the final text.
    fn two_tool_turn() -> Vec<Message> {
        vec![
            Message::user("read both"),
            tool_call("call-1", Some("Let me look.")),
            tool_result("call-1"),
            tool_call("call-2", None),
            tool_result("call-2"),
            Message::assistant("Let me look.\n\nBoth read."),
        ]
    }

    fn finalize(entries: &mut Vec<MessageEntry>, turn: Option<Vec<Message>>) {
        finalize_response_state(
            entries,
            turn,
            "Let me look.\n\nBoth read.".to_string(),
            vec![PathBuf::from("/tmp/chart.png")],
            Some(serde_json::json!({ "items": [] })),
            1_700_000_000,
        );
    }

    fn messages(entries: &[MessageEntry]) -> Vec<&Message> {
        entries.iter().map(|e| &e.message).collect()
    }

    /// Every tool result must directly follow the assistant message holding
    /// its call: OpenAI-compatible endpoints reject orphans on either side.
    fn assert_pairs_intact(entries: &[MessageEntry]) {
        use rig_core::completion::message::UserContent;
        for (i, entry) in entries.iter().enumerate() {
            let Message::User { content } = &entry.message else {
                continue;
            };
            for item in content {
                let UserContent::ToolResult(result) = item else {
                    continue;
                };
                let previous = i
                    .checked_sub(1)
                    .map(|p| &entries[p].message)
                    .unwrap_or_else(|| panic!("tool result at index {i} has nothing before it"));
                let Message::Assistant { content, .. } = previous else {
                    panic!("tool result at index {i} does not follow an assistant message");
                };
                assert!(
                    content.iter().any(|ac| matches!(
                        ac,
                        AssistantContent::ToolCall(tc) if tc.id.as_str() == result.call.as_str()
                    )),
                    "tool result {} at index {i} does not follow its call",
                    result.call.as_str()
                );
            }
        }
    }

    #[test]
    fn a_turn_with_two_tool_calls_persists_five_messages_in_order() {
        let mut entries = vec![entry(Message::user("read both"), None)];
        finalize(&mut entries, Some(two_tool_turn()));

        // user prompt + 2 × (call, result) + final text
        assert_eq!(entries.len(), 6);
        let shape: Vec<&str> = messages(&entries)
            .iter()
            .map(|m| match m {
                Message::User { .. } if is_tool_result_message(m) => "result",
                Message::User { .. } => "user",
                Message::Assistant { .. } if crate::services::is_tool_call_message(m) => "call",
                Message::Assistant { .. } => "text",
                Message::System { .. } => "system",
            })
            .collect();
        assert_eq!(shape, ["user", "call", "result", "call", "result", "text"]);
        assert_pairs_intact(&entries);

        // The trace and attachments stay on the final text entry only.
        for e in &entries[1..5] {
            assert!(e.system_trace.is_none());
            assert!(e.attachment_paths.is_empty());
            assert_eq!(e.timestamp, Some(1_700_000_000));
        }
        assert!(entries[5].system_trace.is_some());
        assert_eq!(entries[5].attachment_paths.len(), 1);

        // This is the history the next turn hands to `stream_prompt`
        // (`Conversation::messages()` clones exactly these), so the second
        // request carries the first turn's tool messages.
        let history: Vec<Message> = messages(&entries).into_iter().cloned().collect();
        assert!(history.iter().any(crate::services::is_tool_call_message));
        assert!(history.iter().any(is_tool_result_message));
    }

    #[test]
    fn persisted_tool_turns_round_trip_through_the_history_json() {
        let mut entries = vec![entry(Message::user("read both"), None)];
        finalize(&mut entries, Some(two_tool_turn()));

        let history: Vec<&Message> = messages(&entries);
        let json = serde_json::to_string(&history).unwrap();
        let reloaded = Conversation::deserialize_history(&json).unwrap();

        assert_eq!(reloaded.len(), entries.len());
        assert_eq!(
            serde_json::to_value(&reloaded).unwrap(),
            serde_json::to_value(&history).unwrap(),
            "tool calls and results must reload identically"
        );
        let reloaded_entries: Vec<MessageEntry> =
            reloaded.into_iter().map(|m| entry(m, None)).collect();
        assert_pairs_intact(&reloaded_entries);
    }

    #[test]
    fn a_trailing_tool_call_without_a_result_is_not_persisted() {
        // A run cut off at its turn limit ends on a call rig never executed.
        let mut turn = two_tool_turn();
        turn.pop();
        turn.push(tool_call("call-3", None));

        let mut entries = vec![entry(Message::user("read both"), None)];
        finalize(&mut entries, Some(turn));

        assert_eq!(entries.len(), 6);
        assert!(!crate::services::is_tool_call_message(&entries[5].message));
        assert_pairs_intact(&entries);
    }

    #[test]
    fn finalize_without_a_turn_record_persists_only_the_text() {
        let mut entries = vec![entry(Message::user("hi"), None)];
        finalize(&mut entries, None);
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[1].message, Message::Assistant { .. }));

        // A record with no tool activity adds nothing either.
        let mut entries = vec![entry(Message::user("hi"), None)];
        finalize(
            &mut entries,
            Some(vec![Message::user("hi"), Message::assistant("hello")]),
        );
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn regeneration_removes_the_whole_previous_turn() {
        let mut entries = vec![
            entry(Message::user("first"), None),
            entry(Message::assistant("1"), None),
            entry(Message::user("read both"), None),
        ];
        finalize(&mut entries, Some(two_tool_turn()));
        assert_eq!(entries.len(), 8);

        let (text, timestamp) = remove_last_turn_state(&mut entries).expect("an answer to remove");
        assert_eq!(text, "Let me look.\n\nBoth read.");
        assert_eq!(timestamp, Some(1_700_000_000));
        // Back to the user message that started the turn, ready to re-stream.
        assert_eq!(entries.len(), 3);
        assert!(is_user_text_message(&entries[2].message));
        assert!(matches!(entries[1].message, Message::Assistant { .. }));
    }

    #[test]
    fn regeneration_of_a_plain_turn_still_removes_one_message() {
        let mut entries = vec![
            entry(Message::user("hi"), None),
            entry(Message::assistant("hello"), None),
        ];
        assert_eq!(
            remove_last_turn_state(&mut entries),
            Some(("hello".to_string(), None))
        );
        assert_eq!(entries.len(), 1);

        // Nothing to remove when the last message is not an answer.
        assert_eq!(remove_last_turn_state(&mut entries), None);
    }
}
