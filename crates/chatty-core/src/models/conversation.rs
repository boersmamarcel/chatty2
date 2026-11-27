use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::trace;

use rig_core::completion::Message;
use rig_core::completion::message::{AssistantContent, Text, UserContent};

use crate::factories::AgentClient;
use crate::factories::agent_factory::{AgentBuildContext, ollama_think};
use crate::models::history_compat;
use crate::models::message_types::{SystemTrace, ToolSource, TraceItem};
use crate::models::token_usage::{ConversationTokenUsage, ModelRef, TokenPricing, TokenUsage};
use crate::repositories::ConversationData;
use crate::services::AgentTaskSnapshot;
use crate::services::shell_service::ShellSession;
use crate::services::{enforce_tool_round_trips, is_tool_result_message};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;
use crate::tools::PendingArtifacts;

/// User feedback signal for an individual assistant message
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessageFeedback {
    ThumbsUp,
    ThumbsDown,
}

/// Is this conversation kept on the user's Hive account, and if so, where do
/// its turns run (AGE-2088, plan D1)?
///
/// Two questions that `ConversationMode` used to answer with one enum:
///
/// * **sync** — [`LocalOnly`](Self::LocalOnly) never leaves the machine;
///   [`Synced`](Self::Synced) means the server holds the canonical copy and
///   this device keeps a cache of it (D3).
/// * **placement** — only a synced conversation has one: where its turns run
///   right now ([`Placement`]). Nesting it inside `Synced` is what makes "a
///   local-only conversation running in the cloud" unrepresentable.
///
/// Syncing alone never runs anything remotely; "send to the cloud" changes
/// the placement, not the sync.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConversationSync {
    /// Never leaves this machine. The default, and what every row written
    /// before sync existed loads as.
    #[default]
    LocalOnly,
    /// Kept on the server; this device holds a cache.
    Synced(SyncedConversation),
}

/// A synced conversation's address on the server and how current this
/// device's copy of it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedConversation {
    /// Base URL of the server, without a trailing slash. One per account once
    /// the Hive URL is unified (D5); carried per conversation until then,
    /// because the move dialog still names a server per move.
    pub server_url: String,
    /// The conversation's id *on that server*, which need not be the local id.
    pub remote_id: String,
    /// The server version this device's copy of the *history* is at. A newer
    /// version in the server's list marks the detail stale; it is replaced
    /// when the detail is fetched again. `0` means none was fetched yet.
    pub version: u64,
    /// Where turns run.
    pub placement: Placement,
    /// When (unix ms) this device last brought its copy of the history in
    /// line with the server; `None` while the cached detail is stale or was
    /// never fetched.
    pub last_synced_at: Option<i64>,
}

/// Where a synced conversation's turns run right now (D1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Placement {
    /// In-process on the named device, with its local models and workspace.
    Device { device_id: String },
    /// On chatty-server; every device is a viewer and composer.
    Cloud {
        /// The model the server reported for the conversation (AGE-849),
        /// shown in the composer in place of the local pick. `None` when the
        /// server named none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_id: Option<String>,
    },
}

/// The `sync` column's JSON: the synced conversation's address. The version,
/// placement and sync time have columns of their own.
#[derive(Serialize, Deserialize)]
struct SyncColumn {
    server_url: String,
    remote_id: String,
}

impl ConversationSync {
    /// A conversation whose turns run on `server_url` as `remote_id`: what a
    /// move online produces.
    pub fn cloud(
        server_url: impl Into<String>,
        remote_id: impl Into<String>,
        model_id: Option<String>,
    ) -> Self {
        ConversationSync::Synced(SyncedConversation {
            server_url: server_url.into().trim_end_matches('/').to_string(),
            remote_id: remote_id.into(),
            version: 0,
            placement: Placement::Cloud { model_id },
            last_synced_at: None,
        })
    }

    /// The synced half, if any.
    pub fn synced(&self) -> Option<&SyncedConversation> {
        match self {
            ConversationSync::LocalOnly => None,
            ConversationSync::Synced(synced) => Some(synced),
        }
    }

    /// Where turns run; `None` for a local-only conversation, whose turns
    /// always run here.
    pub fn placement(&self) -> Option<&Placement> {
        self.synced().map(|synced| &synced.placement)
    }

    /// The server this conversation's turns run on, when its placement is
    /// [`Cloud`](Placement::Cloud): `(server_url, remote_id)`.
    pub fn cloud_on(&self) -> Option<(&str, &str)> {
        match self.synced() {
            Some(SyncedConversation {
                server_url,
                remote_id,
                placement: Placement::Cloud { .. },
                ..
            }) => Some((server_url.as_str(), remote_id.as_str())),
            _ => None,
        }
    }

    /// Do this conversation's turns run on a server?
    pub fn runs_in_cloud(&self) -> bool {
        self.cloud_on().is_some()
    }

    /// What the composer's model chip says while turns run in the cloud: the
    /// server's model as a person reads it ("Claude Sonnet 5.5"), or
    /// "Default cloud model" when it reported none. `None` when turns run
    /// here, whose chip is the local picker (AGE-849).
    pub fn cloud_model_label(&self) -> Option<String> {
        match self.placement() {
            Some(Placement::Cloud { model_id }) => Some(
                model_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(readable_model_name)
                    .unwrap_or_else(|| "Default cloud model".to_string()),
            ),
            _ => None,
        }
    }

    /// Read the four sync columns of a row. All `NULL` is
    /// [`LocalOnly`](Self::LocalOnly), which is what a row that never synced
    /// holds, so it keeps exactly the bytes it had. A `sync` column that does
    /// not parse is treated the same way: refusing to load a conversation
    /// over it would lose its history. A synced row with no readable
    /// placement is taken to run in the cloud, the only placement a row could
    /// have before devices held one.
    pub fn from_columns(
        sync: Option<&str>,
        placement: Option<&str>,
        remote_version: Option<i64>,
        last_synced_at: Option<i64>,
    ) -> Self {
        let Some(column) = sync.and_then(|json| serde_json::from_str::<SyncColumn>(json).ok())
        else {
            return ConversationSync::LocalOnly;
        };
        let placement = placement
            .and_then(|json| serde_json::from_str::<Placement>(json).ok())
            .unwrap_or(Placement::Cloud { model_id: None });
        ConversationSync::Synced(SyncedConversation {
            server_url: column.server_url,
            remote_id: column.remote_id,
            version: remote_version.unwrap_or(0).max(0) as u64,
            placement,
            last_synced_at,
        })
    }

    /// The four sync columns for a row: all `None` for a local-only one.
    pub fn to_columns(&self) -> SyncColumns {
        match self {
            ConversationSync::LocalOnly => SyncColumns::default(),
            ConversationSync::Synced(synced) => SyncColumns {
                sync: serde_json::to_string(&SyncColumn {
                    server_url: synced.server_url.clone(),
                    remote_id: synced.remote_id.clone(),
                })
                .ok(),
                placement: serde_json::to_string(&synced.placement).ok(),
                remote_version: Some(synced.version.min(i64::MAX as u64) as i64),
                last_synced_at: synced.last_synced_at,
            },
        }
    }

    /// Read the sync columns off a repository row.
    pub fn of_data(data: &ConversationData) -> Self {
        Self::from_columns(
            data.sync.as_deref(),
            data.placement.as_deref(),
            data.remote_version,
            data.last_synced_at,
        )
    }
}

/// The persisted form of a [`ConversationSync`], one field per column.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncColumns {
    pub sync: Option<String>,
    pub placement: Option<String>,
    pub remote_version: Option<i64>,
    pub last_synced_at: Option<i64>,
}

/// Outcome of `Conversation::finalize_turn` (AGE-243 / D4).
#[derive(Clone, Debug, PartialEq)]
pub enum TurnOutcome {
    /// The turn produced response text or a trace and was persisted to
    /// history, along with its tool round-trips when rig recorded any
    /// (AGE-247).
    Persisted,
    /// The turn produced neither, so it was dropped: the pending user message
    /// that triggered it was rolled back rather than left dangling with no
    /// response. Carries that message's text so the caller can put it back
    /// into the composer/input.
    DroppedAndRolledBack(String),
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
    agent: Arc<AgentClient>,
    /// All messages with their metadata, in chronological order.
    entries: Vec<MessageEntry>,
    /// Regeneration records capturing original responses before replacement (DPO preference pairs)
    regeneration_records: Vec<RegenerationRecord>,
    token_usage: ConversationTokenUsage,
    /// Tool calls made over the conversation's lifetime, accumulated at the
    /// turn barrier (AGE-351).
    tool_call_count: u32,
    /// Prompt size of the last completed API call (AGE-351).
    context_tokens: u32,
    /// What a turn on the bound model is costed at; `None` when the model
    /// has no prices, in which case turns carry no cost (AGE-351). Bound
    /// with the agent, so it follows a model switch.
    pricing: Option<TokenPricing>,
    /// The bound model, which a turn's own usage lines are spent on
    /// (AGE-682). Bound with the agent, like `pricing`.
    model_ref: ModelRef,
    /// Whether the bound model's `extra_params.think` is `"false"`: picks
    /// the empty-completion error text (AGE-404). Bound with the agent.
    think_disabled: bool,
    created_at: SystemTime,
    updated_at: SystemTime,
    /// Partial streaming message being composed (None if no active stream)
    streaming_message: Option<String>,
    /// Partial streaming trace being composed (None if no active stream)
    streaming_trace: Option<SystemTrace>,
    /// Transient sub-agent progress traces, each shown as a separate
    /// in-progress UI message, in the order they opened and keyed by the
    /// `invoke_agent` call that made them ([`UNTAGGED_CALL`] when the source
    /// names none). Kept in-memory so switching conversations during an
    /// active sub-agent run can restore the traces and their source badges.
    streaming_delegation_traces: Vec<(u64, SystemTrace)>,
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
    /// Whether the conversation is kept on the Hive account, and where its
    /// turns run (AGE-2088). `LocalOnly` for every conversation that never
    /// synced.
    sync: ConversationSync,
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
        let built = AgentClient::from_model_config_with_tools(model_config, provider_config, ctx)
            .await
            .context("Failed to create agent from config")?;

        let now = SystemTime::now();

        Ok(Self {
            id,
            title,
            model_id: model_config.id.clone(),
            agent: Arc::new(built.client),
            entries: Vec::new(),
            regeneration_records: Vec::new(),
            token_usage: ConversationTokenUsage::new(),
            tool_call_count: 0,
            context_tokens: 0,
            pricing: model_config.token_pricing(),
            model_ref: model_config.model_ref(),
            think_disabled: ollama_think(model_config) == Some(false),
            created_at: now,
            updated_at: now,
            streaming_message: None,
            streaming_trace: None,
            streaming_delegation_traces: Vec::new(),
            streaming_turn_messages: None,
            pending_artifacts,
            shell_session: built.shell_session,
            working_dir: None,
            agent_task_snapshot: None,
            agent_workspace_dir,
            invoke_agent_progress_slot: built.invoke_agent_progress_slot,
            sync: ConversationSync::LocalOnly,
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
        let built = AgentClient::from_model_config_with_tools(model_config, provider_config, ctx)
            .await
            .context("Failed to create agent from config")?;

        let entries = entries_of(&data)?;

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
        let sync = ConversationSync::of_data(&data);

        // Convert Unix timestamps to SystemTime
        let created_at = UNIX_EPOCH + Duration::from_secs(data.created_at as u64);
        let updated_at = UNIX_EPOCH + Duration::from_secs(data.updated_at as u64);

        Ok(Self {
            id: data.id,
            title: data.title,
            model_id: data.model_id,
            agent: Arc::new(built.client),
            entries,
            regeneration_records,
            token_usage,
            tool_call_count: data.tool_call_count,
            context_tokens: data.context_tokens,
            pricing: model_config.token_pricing(),
            model_ref: model_config.model_ref(),
            think_disabled: ollama_think(model_config) == Some(false),
            created_at,
            updated_at,
            streaming_message: None, // Always start fresh, streaming state is transient
            streaming_trace: None,
            streaming_delegation_traces: Vec::new(),
            streaming_turn_messages: None,
            pending_artifacts,
            shell_session: built.shell_session,
            working_dir: data.working_dir.map(PathBuf::from),
            agent_task_snapshot,
            agent_workspace_dir,
            invoke_agent_progress_slot: built.invoke_agent_progress_slot,
            sync,
        })
    }

    /// Add user message to history with attachment paths
    pub fn add_user_message_with_attachments(
        &mut self,
        message: Message,
        attachments: Vec<PathBuf>,
    ) {
        let now = SystemTime::now();
        let timestamp = unix_seconds(now);
        self.entries.push(MessageEntry {
            message,
            system_trace: None,
            attachment_paths: attachments,
            timestamp: Some(timestamp),
            feedback: None,
        });
        self.updated_at = now;
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
        let timestamp = unix_seconds(now);
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

    /// Finalize a turn's response using the one shared empty-turn rule
    /// (AGE-243 / D4): persist when the response carries text or a trace,
    /// otherwise treat the turn as empty and roll back the pending user
    /// message that triggered it — a dangling unanswered user message would
    /// be rejected by the provider on the next request. Both frontends'
    /// finalizers (desktop's `finalize_completed_stream` /
    /// `finalize_stopped_stream`, TUI's `finalize_partial_response`) call
    /// this instead of each implementing their own drop-vs-persist rule.
    ///
    /// A persisted turn also persists rig's record of its tool round-trips
    /// ahead of the final text, when the stream delivered one
    /// (`set_streaming_turn_messages`, AGE-247). The record is taken on both
    /// outcomes, so a dropped turn discards it rather than leaking it into
    /// the next one.
    pub fn finalize_turn(
        &mut self,
        text: String,
        artifacts: Vec<PathBuf>,
        trace: Option<serde_json::Value>,
    ) -> TurnOutcome {
        let outcome = finalize_turn_state(
            &mut self.entries,
            self.streaming_turn_messages.take(),
            text,
            artifacts,
            trace,
        );
        self.updated_at = SystemTime::now();
        outcome
    }

    /// [`finalize_turn`](Self::finalize_turn) for a turn a stall or a
    /// connection failure ended (AGE-1438; see
    /// [`kept_failed_turn_note`](crate::services::kept_failed_turn_note)).
    /// Its round-trips are persisted as on any turn, and so is the user
    /// message of a turn that produced nothing, so a "continue" has the task
    /// and the work in context. A turn with no text of its own gets `note`
    /// as its reply, so the history still alternates and never ends on an
    /// empty assistant text, which some providers reject. `note` is a fixed
    /// sentence, never the error's own text: that can carry a provider's
    /// raw body or a request URL, and the history is saved, shown and sent
    /// back to the provider. Always `Persisted`, except when there is
    /// nothing at all to keep.
    pub fn finalize_failed_turn(
        &mut self,
        text: String,
        artifacts: Vec<PathBuf>,
        trace: Option<serde_json::Value>,
        note: &str,
    ) -> TurnOutcome {
        let outcome = finalize_failed_turn_state(
            &mut self.entries,
            self.streaming_turn_messages.take(),
            text,
            artifacts,
            trace,
            note,
        );
        self.updated_at = SystemTime::now();
        outcome
    }

    /// The last entry's text when it is the fixed note a kept failed turn was
    /// persisted with (AGE-1438), for a frontend whose live view has to
    /// show it: the stream never carried it.
    pub fn kept_failed_turn_note(&self) -> Option<String> {
        let entry = self.entries.last()?;
        let Message::Assistant(rig_core::message::AssistantMessage { content, .. }) =
            &entry.message
        else {
            return None;
        };
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        crate::services::is_kept_failed_turn_note(&text).then_some(text)
    }

    /// Keep rig's record of the turn in flight until the turn is finalized.
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

    /// Deserialize message history from JSON string.
    ///
    /// Accepts the legacy untagged assistant content block an older rig wrote
    /// (AGE-369); see [`history_compat`](crate::models::history_compat).
    pub fn deserialize_history(json: &str) -> Result<Vec<Message>> {
        history_compat::parse_history(json).context("Failed to deserialize message history")
    }

    /// Deserialize system traces from JSON string
    pub fn deserialize_traces(json: &str) -> Result<Vec<Option<serde_json::Value>>> {
        serde_json::from_str(json).context("Failed to deserialize system traces")
    }

    /// Deserialize attachment paths from JSON string
    pub fn deserialize_attachment_paths(json: &str) -> Result<Vec<Vec<PathBuf>>> {
        serde_json::from_str(json).context("Failed to deserialize attachment paths")
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

    /// Deserialize message feedback from JSON string
    pub fn deserialize_message_feedback(json: &str) -> Result<Vec<Option<MessageFeedback>>> {
        serde_json::from_str(json).context("Failed to deserialize message feedback")
    }

    /// Deserialize the persisted agent task snapshot from JSON.
    pub fn deserialize_agent_task_snapshot(json: &str) -> Result<AgentTaskSnapshot> {
        serde_json::from_str(json).context("Failed to deserialize agent task snapshot")
    }

    /// Whether this conversation is synced, and where its turns run.
    pub fn sync(&self) -> &ConversationSync {
        &self.sync
    }

    /// Record a new sync state: a move between this device and the cloud, or
    /// a pull that brought this copy in line with the server.
    ///
    /// The caller owns the transfer of history; this only records the
    /// outcome, and is the last step of a move so a failed transfer leaves
    /// the conversation exactly as it was.
    pub fn set_sync(&mut self, sync: ConversationSync) {
        self.sync = sync;
        self.updated_at = SystemTime::now();
    }

    /// Get regeneration records for this conversation
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
        let now = unix_seconds(SystemTime::now());

        self.regeneration_records.push(RegenerationRecord {
            message_index,
            original_text,
            original_timestamp,
            regeneration_timestamp: now,
        });
        self.updated_at = SystemTime::now();
    }

    /// Deserialize regeneration records from JSON string
    pub fn deserialize_regeneration_records(json: &str) -> Result<Vec<RegenerationRecord>> {
        serde_json::from_str(json).context("Failed to deserialize regeneration records")
    }

    /// A cheap owned copy of everything [`ConversationData`] is built from.
    ///
    /// Clones entries and metadata; runs no serde. Take this on the thread
    /// that owns the conversation, then call
    /// [`ConversationSnapshot::to_data`] wherever the CPU is cheaper — which
    /// is what the desktop does to keep `serde_json` off the UI thread
    /// (AGE-220, finding F3).
    pub fn snapshot(&self) -> ConversationSnapshot {
        ConversationSnapshot {
            id: self.id.clone(),
            title: self.title.clone(),
            model_id: self.model_id.clone(),
            entries: self.entries.clone(),
            token_usage: self.token_usage.clone(),
            tool_call_count: self.tool_call_count,
            context_tokens: self.context_tokens,
            regeneration_records: self.regeneration_records.clone(),
            created_at: self.created_at,
            working_dir: self.working_dir.clone(),
            agent_task_snapshot: self.agent_task_snapshot.clone(),
            sync: self.sync.clone(),
        }
    }

    /// Serialize straight into the repository's row shape.
    ///
    /// Convenience for callers that are not on a latency-sensitive thread;
    /// [`snapshot`](Self::snapshot) plus
    /// [`to_data`](ConversationSnapshot::to_data) is the two-phase form.
    pub fn to_conversation_data(&self) -> Result<ConversationData> {
        self.snapshot().to_data()
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

    /// Adopt a history that came from somewhere else (AGE-298).
    ///
    /// Both directions of a local↔hosted move end here: `chatty-server`
    /// importing a conversation the user took online, and this end rebuilding
    /// one it brought back. The messages are the whole payload — traces,
    /// attachments and feedback are per-message metadata that the wire does
    /// not carry, so they start empty rather than being invented, which is
    /// what AGE-298's table means by saying attachments do not move.
    ///
    /// Distinct from [`replace_history`](Self::replace_history), which is
    /// summarization: that one treats index 0 as a generated summary and
    /// carries the surviving tail's metadata across. Importing has no tail to
    /// carry and no summary to special-case, and conflating the two would put
    /// the first imported message's metadata on the wrong row.
    pub fn import_history(&mut self, messages: Vec<Message>) {
        self.entries = messages
            .into_iter()
            .map(|message| MessageEntry {
                message,
                system_trace: None,
                attachment_paths: Vec::new(),
                timestamp: None,
                feedback: None,
            })
            .collect();
        self.updated_at = SystemTime::now();
    }

    /// Adopt the server's copy of a synced conversation (AGE-2088).
    ///
    /// The server is canonical for a synced conversation (plan D3), so its
    /// history, usage and totals replace this device's. Unlike
    /// [`import_history`](Self::import_history), which starts every message's
    /// metadata empty, the server's traces and timestamps are kept.
    ///
    /// Where the message at an index is the same one this device holds, its
    /// trace, timestamp and feedback stay this device's when it has them: the
    /// server sends only a projection of a trace (AGE-2157), and the full one
    /// this device recorded is the richer copy of the same turn. The server's
    /// fill the messages this device has none for. Metadata never lands on a
    /// different message. Usage the server did not send is kept, and so is
    /// usage that holds delegated-agent or plugin lines, which the server's
    /// projection cannot carry. The creation time stays this device's when
    /// `data` has none (`created_at` 0).
    ///
    /// Attachment paths never come from `data` (AGE-2143): a path the server
    /// names is not a file on this device, and must never become one this
    /// device reads or uploads. A message keeps this device's attachments
    /// only where it is the same message.
    ///
    /// What stays this device's own: the local id, the bound model and agent,
    /// the working directory, and the sync state, which the caller sets once
    /// it knows what version it adopted.
    pub fn replace_from_remote(&mut self, data: ConversationData) -> Result<()> {
        let mut entries = entries_of(&data)?;
        for entry in &mut entries {
            entry.attachment_paths = Vec::new();
        }
        for (entry, local) in entries.iter_mut().zip(&self.entries) {
            if !same_message(&entry.message, &local.message) {
                continue;
            }
            entry.attachment_paths = local.attachment_paths.clone();
            if local.system_trace.is_some() {
                entry.system_trace = local.system_trace.clone();
            }
            if local.timestamp.is_some() {
                entry.timestamp = local.timestamp;
            }
            if local.feedback.is_some() {
                entry.feedback = local.feedback.clone();
            }
        }

        self.entries = entries;
        if !data.title.is_empty() {
            self.title = data.title;
        }
        // The server's usage is a projection of each turn's own lines;
        // delegated-agent and plugin lines are not in it (AGE-2157). A local
        // record holding any of those is the fuller one: it stays.
        let local_has_unprojected = self
            .token_usage
            .message_usages
            .iter()
            .any(|line| line.delegated_to.is_some() || line.plugin.is_some());
        if !local_has_unprojected
            && let Ok(usage) = Self::deserialize_token_usage(&data.token_usage)
            && !usage.message_usages.is_empty()
        {
            self.token_usage = usage;
        }
        if let Ok(records) = Self::deserialize_regeneration_records(&data.regeneration_records)
            && !records.is_empty()
        {
            self.regeneration_records = records;
        }
        if let Some(snapshot) = data
            .agent_task_snapshot
            .as_deref()
            .and_then(|json| Self::deserialize_agent_task_snapshot(json).ok())
        {
            self.agent_task_snapshot = Some(snapshot);
        }
        if data.tool_call_count > 0 {
            self.tool_call_count = data.tool_call_count;
        }
        if data.context_tokens > 0 {
            self.context_tokens = data.context_tokens;
        }
        if data.created_at > 0 {
            self.created_at = UNIX_EPOCH + Duration::from_secs(data.created_at as u64);
        }
        self.updated_at = if data.updated_at > 0 {
            UNIX_EPOCH + Duration::from_secs(data.updated_at as u64)
        } else {
            SystemTime::now()
        };
        Ok(())
    }

    /// Get the agent. Returns a cheap `Arc` clone rather than a deep copy of
    /// rig's `AgentConfig` (finding B1, AGE-219).
    pub fn agent(&self) -> Arc<AgentClient> {
        self.agent.clone()
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

    /// Set the agent and model synchronously (for model switching without
    /// blocking). The model's prices come with it, so the next turn is
    /// costed at the new model's rates.
    pub fn set_agent(
        &mut self,
        agent: Arc<AgentClient>,
        model_config: &ModelConfig,
        agent_workspace_dir: Option<PathBuf>,
    ) {
        self.agent = agent;
        self.model_id = model_config.id.clone();
        self.pricing = model_config.token_pricing();
        self.model_ref = model_config.model_ref();
        self.think_disabled = ollama_think(model_config) == Some(false);
        self.agent_workspace_dir = agent_workspace_dir;
        self.updated_at = SystemTime::now();
    }

    /// Get token usage stats
    pub fn token_usage(&self) -> &ConversationTokenUsage {
        &self.token_usage
    }

    /// What a turn on the bound model is costed at, if it has prices.
    pub fn pricing(&self) -> Option<&TokenPricing> {
        self.pricing.as_ref()
    }

    /// The bound model: what a turn's own usage is spent on (AGE-682).
    pub fn model_ref(&self) -> &ModelRef {
        &self.model_ref
    }

    /// Whether the bound model's thinking channel is switched off
    /// (`extra_params.think` = `"false"`; AGE-404).
    pub fn think_disabled(&self) -> bool {
        self.think_disabled
    }

    /// Tool calls made over the conversation's lifetime (AGE-351).
    pub fn tool_call_count(&self) -> u32 {
        self.tool_call_count
    }

    /// Prompt size of the last completed API call: `input + cache_read +
    /// cache_write`, output excluded (AGE-351). 0 until a turn reports usage.
    pub fn context_tokens(&self) -> u32 {
        self.context_tokens
    }

    /// Count a turn's tool calls into the lifetime total (AGE-351).
    pub fn add_tool_calls(&mut self, count: u32) {
        self.tool_call_count = self.tool_call_count.saturating_add(count);
    }

    /// Add token usage for the most recent exchange. The exchange's last
    /// request is the one whose prompt is the current context fill; a record
    /// without per-request usage holds the provider's aggregate as its one
    /// stand-in, so its exchange prompt total is used the same way.
    pub fn add_token_usage(&mut self, usage: TokenUsage) {
        self.context_tokens = usage
            .last_call()
            .map(|call| call.prompt_tokens())
            .unwrap_or_else(|| usage.prompt_tokens());
        self.token_usage.add_usage(usage);
        self.updated_at = SystemTime::now();
    }

    /// Add what a delegated agent spent on this conversation's behalf
    /// (AGE-415): a line on the usage record and the totals, but not the
    /// context fill — that is the worker's prompt, not this agent's.
    pub fn add_delegated_usage(&mut self, usage: TokenUsage) {
        self.token_usage.add_usage(usage);
        self.updated_at = SystemTime::now();
    }

    /// Serialize token usage to JSON string
    pub fn serialize_token_usage(&self) -> Result<String> {
        serde_json::to_string(&self.token_usage).context("Failed to serialize token usage")
    }

    /// Deserialize token usage from JSON string
    pub fn deserialize_token_usage(json: &str) -> Result<ConversationTokenUsage> {
        let mut usage: ConversationTokenUsage =
            serde_json::from_str(json).context("Failed to deserialize token usage")?;
        usage.forget_unattributed_costs();
        Ok(usage)
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

    /// The sub-agent progress traces of the turn in flight, in the order
    /// they opened, each with the call that made it.
    pub fn streaming_delegation_traces(&self) -> &[(u64, SystemTrace)] {
        &self.streaming_delegation_traces
    }

    pub fn clear_streaming_delegation_traces(&mut self) {
        self.streaming_delegation_traces.clear();
    }

    pub fn start_delegation_progress(&mut self, call: u64, prompt: &str, source: ToolSource) {
        start_delegation_progress_state(
            &mut self.streaming_delegation_traces,
            call,
            prompt,
            source,
        );
    }

    pub fn append_delegation_progress(&mut self, call: u64, line: &str) {
        append_delegation_progress_state(&mut self.streaming_delegation_traces, call, line);
    }

    pub fn finalize_delegation_progress(
        &mut self,
        call: u64,
        success: bool,
        result: Option<String>,
    ) {
        finalize_delegation_progress_state(
            &mut self.streaming_delegation_traces,
            call,
            success,
            result,
        );
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

// ── Snapshot → ConversationData ──────────────────────────────────────────────

/// Everything [`ConversationData`] is built from, owned and detached from the
/// live [`Conversation`].
///
/// This exists so the projection lives in one place. Before it, chatty-gpui
/// assembled `ConversationData` two different ways in two files and
/// chatty-server a third; a field added to `ConversationData` was silently
/// dropped by whichever copy the author did not open (AGE-293).
#[derive(Clone, Debug)]
pub struct ConversationSnapshot {
    pub id: String,
    pub title: String,
    pub model_id: String,
    pub entries: Vec<MessageEntry>,
    pub token_usage: ConversationTokenUsage,
    pub tool_call_count: u32,
    pub context_tokens: u32,
    pub regeneration_records: Vec<RegenerationRecord>,
    pub created_at: SystemTime,
    pub working_dir: Option<PathBuf>,
    pub agent_task_snapshot: Option<AgentTaskSnapshot>,
    pub sync: ConversationSync,
}

impl ConversationSnapshot {
    /// Serialize into the repository's row shape.
    ///
    /// Pure CPU over owned data: safe to call from a background task.
    /// `updated_at` is stamped here, at the moment the row is built.
    pub fn to_data(&self) -> Result<ConversationData> {
        let columns = self.sync.to_columns();
        Ok(ConversationData {
            id: self.id.clone(),
            title: self.title.clone(),
            model_id: self.model_id.clone(),
            message_history: serialize_history_of(&self.entries)?,
            system_traces: serialize_traces_of(&self.entries)?,
            token_usage: serde_json::to_string(&self.token_usage)
                .context("Failed to serialize token usage")?,
            attachment_paths: serialize_attachment_paths_of(&self.entries)?,
            message_timestamps: serialize_message_timestamps_of(&self.entries)?,
            message_feedback: serialize_message_feedback_of(&self.entries)?,
            regeneration_records: serde_json::to_string(&self.regeneration_records)
                .context("Failed to serialize regeneration records")?,
            created_at: unix_seconds(self.created_at),
            updated_at: unix_seconds(SystemTime::now()),
            working_dir: self
                .working_dir
                .as_ref()
                .map(|dir| dir.to_string_lossy().into_owned()),
            agent_task_snapshot: self
                .agent_task_snapshot
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .context("Failed to serialize agent task snapshot")?,
            // Local-only is all `NULL`, so a conversation that never syncs
            // keeps the bytes it had before sync existed.
            sync: columns.sync,
            placement: columns.placement,
            remote_version: columns.remote_version,
            last_synced_at: columns.last_synced_at,
            tool_call_count: self.tool_call_count,
            context_tokens: self.context_tokens,
        })
    }
}

/// The per-message entries a repository row describes: the history zipped
/// with its traces, attachments, timestamps and feedback. The metadata arrays
/// fall back to empty when they do not parse, as rows from before they
/// existed carry none.
fn entries_of(data: &ConversationData) -> Result<Vec<MessageEntry>> {
    // Deserialize message history
    let history = Conversation::deserialize_history(&data.message_history)
        .context("Failed to deserialize message history")?;

    // Deserialize system traces
    let system_traces = Conversation::deserialize_traces(&data.system_traces)
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
        Conversation::deserialize_attachment_paths(&data.attachment_paths).unwrap_or_default();
    let message_timestamps =
        Conversation::deserialize_message_timestamps(&data.message_timestamps).unwrap_or_default();
    let message_feedback =
        Conversation::deserialize_message_feedback(&data.message_feedback).unwrap_or_default();

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
    Ok(entries)
}

fn unix_seconds(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

// The per-field projections, shared by `Conversation`'s `serialize_*` methods
// and by `ConversationSnapshot::to_data` so there is exactly one of each.

fn serialize_history_of(entries: &[MessageEntry]) -> Result<String> {
    let messages: Vec<&Message> = entries.iter().map(|e| &e.message).collect();
    serde_json::to_string(&messages).context("Failed to serialize message history")
}

fn serialize_traces_of(entries: &[MessageEntry]) -> Result<String> {
    let traces: Vec<Option<&serde_json::Value>> =
        entries.iter().map(|e| e.system_trace.as_ref()).collect();
    serde_json::to_string(&traces).context("Failed to serialize system traces")
}

fn serialize_attachment_paths_of(entries: &[MessageEntry]) -> Result<String> {
    let paths: Vec<&Vec<PathBuf>> = entries.iter().map(|e| &e.attachment_paths).collect();
    serde_json::to_string(&paths).context("Failed to serialize attachment paths")
}

fn serialize_message_timestamps_of(entries: &[MessageEntry]) -> Result<String> {
    let timestamps: Vec<Option<i64>> = entries.iter().map(|e| e.timestamp).collect();
    serde_json::to_string(&timestamps).context("Failed to serialize message timestamps")
}

fn serialize_message_feedback_of(entries: &[MessageEntry]) -> Result<String> {
    let feedback: Vec<Option<&MessageFeedback>> =
        entries.iter().map(|e| e.feedback.as_ref()).collect();
    serde_json::to_string(&feedback).context("Failed to serialize message feedback")
}

/// Are two messages the same one? Compared through serde, which is how they
/// are persisted, so a message is "the same" exactly when its row would be.
fn same_message(a: &Message, b: &Message) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
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
        .position(|m| matches!(m, Message::Assistant(_)))
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
    // AGE-485: a turn a loop-guard pivot or a user cancel ended mid-batch can
    // hand back a slice whose assistant `tool_calls` entry outran its
    // sibling results; repair it before it is ever persisted, since every
    // OpenAI-compatible provider rejects the mismatch on the *next* request.
    let tool_messages = turn_messages
        .map(turn_tool_messages)
        .map(|messages| enforce_tool_round_trips(messages, &[]))
        .unwrap_or_default();
    entries.extend(tool_messages.into_iter().map(|message| MessageEntry {
        message,
        system_trace: None,
        attachment_paths: Vec::new(),
        timestamp: Some(timestamp),
        feedback: None,
    }));

    entries.push(MessageEntry {
        message: Message::Assistant(rig_core::message::AssistantMessage::new(vec![
            AssistantContent::Text(Text::new(response_text)),
        ])),
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
    let Message::Assistant(rig_core::message::AssistantMessage { content, .. }) = &last.message
    else {
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
        .find(|e| matches!(e.message, Message::Assistant(_)))
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

/// The call key of delegation progress from a source that tags none.
pub const UNTAGGED_CALL: u64 = 0;

fn start_delegation_progress_state(
    traces: &mut Vec<(u64, SystemTrace)>,
    call: u64,
    prompt: &str,
    source: ToolSource,
) {
    let trace = SystemTrace::new_delegation(prompt, source);
    match traces.iter_mut().find(|(open, _)| *open == call) {
        Some((_, slot)) => *slot = trace,
        None => traces.push((call, trace)),
    }
}

fn append_delegation_progress_state(traces: &mut [(u64, SystemTrace)], call: u64, line: &str) {
    if let Some((_, trace)) = traces.iter_mut().find(|(open, _)| *open == call) {
        trace.append_delegation_progress(line);
    }
}

fn finalize_delegation_progress_state(
    traces: &mut [(u64, SystemTrace)],
    call: u64,
    success: bool,
    result: Option<String>,
) {
    if let Some((_, trace)) = traces.iter_mut().find(|(open, _)| *open == call) {
        trace.finalize_delegation_progress(success, result);
    }
}

/// The logic behind `Conversation::finalize_turn`, extracted for direct unit
/// testing (mirrors `append_trace_item_state` et al.): persist an assistant
/// entry when there's text or a trace, otherwise pop the last (user) entry
/// and return its text so the caller can restore it to the composer/input.
fn finalize_turn_state(
    entries: &mut Vec<MessageEntry>,
    turn_messages: Option<Vec<Message>>,
    text: String,
    artifacts: Vec<PathBuf>,
    trace: Option<serde_json::Value>,
) -> TurnOutcome {
    if !text.trim().is_empty() || trace.is_some() {
        let now = SystemTime::now();
        let timestamp = unix_seconds(now);
        // Delegate to the same helper `finalize_response` uses, so the turn's
        // tool round-trips are persisted ahead of the final text entry
        // (AGE-247) by exactly one code path rather than two that can drift.
        finalize_response_state(entries, turn_messages, text, artifacts, trace, timestamp);
        return TurnOutcome::Persisted;
    }

    let rolled_back_text = entries
        .last()
        .map(|e| user_message_text(&e.message))
        .unwrap_or_default();
    if matches!(
        entries.last().map(|e| &e.message),
        Some(Message::User { .. })
    ) {
        entries.pop();
    }
    TurnOutcome::DroppedAndRolledBack(rolled_back_text)
}

/// The logic behind `Conversation::finalize_failed_turn`: a failed turn
/// with no text is persisted with `note` as its text,
/// whenever there is something to keep — its round-trips, its trace, or the
/// user message that started it. The rest is `finalize_turn_state`'s.
fn finalize_failed_turn_state(
    entries: &mut Vec<MessageEntry>,
    turn_messages: Option<Vec<Message>>,
    text: String,
    artifacts: Vec<PathBuf>,
    trace: Option<serde_json::Value>,
    note: &str,
) -> TurnOutcome {
    let something_to_keep = trace.is_some()
        || turn_messages.is_some()
        || matches!(
            entries.last().map(|e| &e.message),
            Some(Message::User { .. })
        );
    let text = if text.trim().is_empty() && something_to_keep {
        note.to_string()
    } else {
        text
    };
    finalize_turn_state(entries, turn_messages, text, artifacts, trace)
}

/// The plain-text parts of a user message, joined with blank lines between
/// parts. Non-text content (images, documents, tool results) is dropped —
/// used to restore what the user typed into the composer/input, not what
/// they attached.
fn user_message_text(message: &Message) -> String {
    match message {
        Message::User { content } => content
            .iter()
            .filter_map(|c| match c {
                UserContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// A model identifier as a person reads it, as the Hive server names its
/// models (hive#822): "claude-sonnet-5-5" is "Claude Sonnet 5.5",
/// "anthropic/claude-opus-5-5" is "Claude Opus 5.5", "qwen3-27b" is
/// "Qwen3 27B". A name that is not an identifier (it has a space) is kept.
pub fn readable_model_name(identifier: &str) -> String {
    if identifier.contains(' ') {
        return identifier.to_string();
    }
    let base = identifier.rsplit('/').next().unwrap_or(identifier);
    let parts: Vec<&str> = base
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .collect();
    let numeric = |part: &&str| part.chars().all(|c| c.is_ascii_digit());
    let version_at = parts
        .iter()
        .rposition(|part| !numeric(part))
        .map_or(0, |last_word| last_word + 1);
    let mut words: Vec<String> = parts[..version_at]
        .iter()
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            let acronym = ["gpt", "glm", "llm"].contains(&lower.as_str());
            let size = lower.len() > 1
                && lower.ends_with(['b', 'k', 'm'])
                && lower[..lower.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.');
            if acronym || size {
                lower.to_ascii_uppercase()
            } else {
                let mut chars = lower.chars();
                chars
                    .next()
                    .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            }
        })
        .collect();
    if version_at < parts.len() {
        words.push(parts[version_at..].join("."));
    }
    if words.is_empty() {
        identifier.to_string()
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::message_types::TraceItem;

    /// A snapshot with every field distinguishable, so a projection wired to
    /// the wrong column shows up rather than passing on shape alone.
    fn populated_snapshot() -> ConversationSnapshot {
        let entries = vec![
            MessageEntry {
                message: Message::user("first"),
                system_trace: Some(serde_json::json!({ "trace": 1 })),
                attachment_paths: vec![PathBuf::from("/tmp/a.png")],
                timestamp: Some(1_700_000_000),
                feedback: Some(MessageFeedback::ThumbsUp),
            },
            MessageEntry {
                message: Message::assistant("second"),
                system_trace: None,
                attachment_paths: Vec::new(),
                timestamp: Some(1_700_000_100),
                feedback: None,
            },
        ];
        let mut token_usage = ConversationTokenUsage::new();
        token_usage.add_usage(TokenUsage::new(11, 22));

        ConversationSnapshot {
            id: "conv-1".to_string(),
            title: "A title".to_string(),
            model_id: "model-1".to_string(),
            entries,
            token_usage,
            tool_call_count: 7,
            context_tokens: 4_321,
            regeneration_records: vec![RegenerationRecord {
                message_index: 1,
                original_text: "before".to_string(),
                original_timestamp: 1_700_000_050,
                regeneration_timestamp: 1_700_000_060,
            }],
            created_at: UNIX_EPOCH + Duration::from_secs(1_699_999_000),
            working_dir: Some(PathBuf::from("/tmp/workspace")),
            agent_task_snapshot: None,
            sync: ConversationSync::cloud("http://localhost:8081", "remote-1", None),
        }
    }

    /// The one construction site has to put each projection in its own
    /// column. Two adjacent `Vec<Option<…>>` fields (traces and feedback)
    /// serialize to the same shape, so a transposition is invisible to the
    /// type system and is exactly what a hand-written copy gets wrong
    /// (AGE-293).
    #[test]
    fn to_data_puts_every_projection_in_its_own_column() {
        let snapshot = populated_snapshot();
        let data = snapshot.to_data().expect("the row serializes");

        assert_eq!(data.id, "conv-1");
        assert_eq!(data.title, "A title");
        assert_eq!(data.model_id, "model-1");
        assert_eq!(data.created_at, 1_699_999_000);
        assert_eq!(data.working_dir.as_deref(), Some("/tmp/workspace"));
        assert!(data.agent_task_snapshot.is_none());

        assert_eq!(
            data.message_history,
            serialize_history_of(&snapshot.entries).unwrap()
        );
        assert_eq!(data.system_traces, r#"[{"trace":1},null]"#);
        assert_eq!(data.attachment_paths, r#"[["/tmp/a.png"],[]]"#);
        assert_eq!(data.message_timestamps, "[1700000000,1700000100]");
        assert_eq!(data.message_feedback, r#"["ThumbsUp",null]"#);
        assert!(data.regeneration_records.contains("before"));
        assert!(data.token_usage.contains("\"total_input_tokens\":11"));
        assert_eq!(data.tool_call_count, 7);
        assert_eq!(data.context_tokens, 4_321);

        // Stamped at build time, not copied from `created_at`.
        assert!(data.updated_at >= data.created_at);
    }

    /// A synced conversation's address and placement have to survive the
    /// row, or "bring this back" would have no server to ask after a restart.
    #[test]
    fn the_sync_state_round_trips_through_the_row() {
        let mut synced = ConversationSync::cloud("http://localhost:8081/", "remote-1", None);
        if let ConversationSync::Synced(s) = &mut synced {
            s.version = 7;
            s.last_synced_at = Some(1_700_000_000_000);
        }
        let data = ConversationSnapshot {
            sync: synced.clone(),
            ..populated_snapshot()
        }
        .to_data()
        .unwrap();
        assert_eq!(data.remote_version, Some(7));
        assert_eq!(ConversationSync::of_data(&data), synced);
        assert_eq!(
            synced.cloud_on(),
            Some(("http://localhost:8081", "remote-1")),
            "the trailing slash is trimmed once, at construction"
        );

        let on_device = ConversationSync::Synced(SyncedConversation {
            server_url: "https://hive.example".into(),
            remote_id: "r-2".into(),
            version: 3,
            placement: Placement::Device {
                device_id: "dev-1".into(),
            },
            last_synced_at: None,
        });
        let data = ConversationSnapshot {
            sync: on_device.clone(),
            ..populated_snapshot()
        }
        .to_data()
        .unwrap();
        assert_eq!(ConversationSync::of_data(&data), on_device);
        assert!(
            !on_device.runs_in_cloud(),
            "a device placement runs its turns on that device"
        );
    }

    /// Local-only is the absent case, so nothing changes on disk for the
    /// conversations that never sync.
    #[test]
    fn a_local_only_conversation_writes_no_sync_columns() {
        let data = ConversationSnapshot {
            sync: ConversationSync::LocalOnly,
            ..populated_snapshot()
        }
        .to_data()
        .unwrap();
        assert!(data.sync.is_none());
        assert!(data.placement.is_none());
        assert!(data.remote_version.is_none());
        assert!(data.last_synced_at.is_none());
    }

    /// A row written before sync existed still loads, as local-only. This is
    /// a property of the *deserializer*, so it is tested through serde and
    /// not through a hand-built struct.
    #[test]
    fn a_row_from_before_sync_loads_as_local_only() {
        let row = serde_json::json!({
            "id": "old-1",
            "title": "Old",
            "model_id": "model-1",
            "message_history": "[]",
            "system_traces": "[]",
            "created_at": 1,
            "updated_at": 2,
        });
        let data: ConversationData = serde_json::from_value(row).expect("an old row still loads");
        assert!(data.sync.is_none());
        // AGE-351: the totals default to 0 on a row from before they existed.
        assert_eq!(data.tool_call_count, 0);
        assert_eq!(data.context_tokens, 0);
        assert_eq!(
            ConversationSync::of_data(&data),
            ConversationSync::LocalOnly
        );
    }

    /// An unreadable `sync` column loads as local-only rather than refusing
    /// the conversation, which would lose its history.
    #[test]
    fn an_unreadable_sync_column_loads_as_local_only() {
        assert_eq!(
            ConversationSync::from_columns(Some("not json"), None, Some(3), None),
            ConversationSync::LocalOnly
        );
    }

    /// `Conversation`'s long-standing per-field `serialize_*` methods and the
    /// new one-shot constructor must agree, since both are public and callers
    /// use each. They share the projections; this pins that they still do.
    #[test]
    fn the_constructor_agrees_with_the_per_field_helpers() {
        let snapshot = populated_snapshot();
        let data = snapshot.to_data().unwrap();
        let entries = &snapshot.entries;

        assert_eq!(data.message_history, serialize_history_of(entries).unwrap());
        assert_eq!(data.system_traces, serialize_traces_of(entries).unwrap());
        assert_eq!(
            data.attachment_paths,
            serialize_attachment_paths_of(entries).unwrap()
        );
        assert_eq!(
            data.message_timestamps,
            serialize_message_timestamps_of(entries).unwrap()
        );
        assert_eq!(
            data.message_feedback,
            serialize_message_feedback_of(entries).unwrap()
        );
    }

    /// An empty conversation still produces the empty-array forms the
    /// repository's serde defaults expect, not `null` or a missing field.
    #[test]
    fn an_empty_conversation_serializes_to_empty_arrays() {
        let data = ConversationSnapshot {
            entries: Vec::new(),
            regeneration_records: Vec::new(),
            working_dir: None,
            ..populated_snapshot()
        }
        .to_data()
        .unwrap();

        assert_eq!(data.message_history, "[]");
        assert_eq!(data.system_traces, "[]");
        assert_eq!(data.attachment_paths, "[]");
        assert_eq!(data.message_timestamps, "[]");
        assert_eq!(data.message_feedback, "[]");
        assert_eq!(data.regeneration_records, "[]");
        assert!(data.working_dir.is_none());
    }

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
    fn deserialize_traces_preserves_running_delegation_source() {
        let mut trace = SystemTrace::new_delegation("review this thread", ToolSource::HiveCloud);
        trace.append_delegation_progress("Working...");

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
        assert!(restored.is_running_delegation());
        assert_eq!(tc.output.as_deref(), Some("Working..."));
    }

    #[test]
    fn delegation_progress_does_not_touch_parent_body() {
        let streaming_message = Some("assistant body".to_string());
        let streaming_trace = Some(SystemTrace::new());
        let mut streaming_delegation_traces = Vec::new();

        start_delegation_progress_state(
            &mut streaming_delegation_traces,
            UNTAGGED_CALL,
            "investigate",
            ToolSource::Local,
        );
        append_delegation_progress_state(
            &mut streaming_delegation_traces,
            UNTAGGED_CALL,
            "working...",
        );
        finalize_delegation_progress_state(
            &mut streaming_delegation_traces,
            UNTAGGED_CALL,
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

        let (_, trace) = streaming_delegation_traces
            .first()
            .expect("sub-agent trace should persist");
        let tc = match &trace.items[0] {
            TraceItem::ToolCall(tc) => tc,
            _ => panic!("expected ToolCall"),
        };
        assert_eq!(tc.output.as_deref(), Some("working...\n\n---\n\ndone"));
        assert!(!trace.is_running_delegation());
    }

    /// Two delegations in flight at once each keep their own card: progress
    /// and the finish go to the call they name, not to the latest one.
    #[test]
    fn parallel_delegations_keep_separate_cards() {
        let mut traces = Vec::new();
        start_delegation_progress_state(&mut traces, 1, "first", ToolSource::Local);
        start_delegation_progress_state(&mut traces, 2, "second", ToolSource::Local);
        append_delegation_progress_state(&mut traces, 1, "reading");
        finalize_delegation_progress_state(&mut traces, 2, true, Some("two done".to_string()));

        assert!(traces[0].1.is_running_delegation());
        assert!(!traces[1].1.is_running_delegation());
        let output = |trace: &SystemTrace| match &trace.items[0] {
            TraceItem::ToolCall(tc) => tc.output.clone(),
            _ => panic!("expected ToolCall"),
        };
        assert_eq!(output(&traces[0].1).as_deref(), Some("reading"));
        assert_eq!(output(&traces[1].1).as_deref(), Some("two done"));
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
            serde_json::to_value(SystemTrace::new_delegation("x", ToolSource::Local)).unwrap();
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
            serde_json::to_value(SystemTrace::new_delegation("x", ToolSource::Local)).unwrap();
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

    // AGE-243 / D4: one shared empty-turn rule, exercised on the four
    // combinations of empty/non-empty text and trace.

    #[test]
    fn finalize_turn_persists_non_empty_text_with_no_trace() {
        let mut entries = vec![entry(Message::user("hi"), None)];

        let outcome = finalize_turn_state(
            &mut entries,
            None,
            "hello there".to_string(),
            Vec::new(),
            None,
        );

        assert_eq!(outcome, TurnOutcome::Persisted);
        assert_eq!(entries.len(), 2);
        match &entries[1].message {
            Message::Assistant(rig_core::message::AssistantMessage { content, .. }) => {
                match &content[0] {
                    AssistantContent::Text(t) => assert_eq!(t.text, "hello there"),
                    other => panic!("expected Text, got {other:?}"),
                }
            }
            other => panic!("expected Assistant message, got {other:?}"),
        }
    }

    #[test]
    fn finalize_turn_persists_empty_text_with_a_trace() {
        let mut entries = vec![entry(Message::user("hi"), None)];
        let trace = serde_json::to_value(SystemTrace::new()).unwrap();

        let outcome = finalize_turn_state(
            &mut entries,
            None,
            String::new(),
            Vec::new(),
            Some(trace.clone()),
        );

        assert_eq!(outcome, TurnOutcome::Persisted);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].system_trace, Some(trace));
    }

    #[test]
    fn finalize_turn_persists_non_empty_text_with_a_trace() {
        let mut entries = vec![entry(Message::user("hi"), None)];
        let trace = serde_json::to_value(SystemTrace::new()).unwrap();

        let outcome = finalize_turn_state(
            &mut entries,
            None,
            "hello".to_string(),
            Vec::new(),
            Some(trace),
        );

        assert_eq!(outcome, TurnOutcome::Persisted);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn finalize_turn_drops_and_rolls_back_empty_text_with_no_trace() {
        let mut entries = vec![entry(Message::user("what happened?"), None)];

        let outcome = finalize_turn_state(&mut entries, None, String::new(), Vec::new(), None);

        assert_eq!(
            outcome,
            TurnOutcome::DroppedAndRolledBack("what happened?".to_string())
        );
        assert!(
            entries.is_empty(),
            "pending user message must be rolled back"
        );
    }

    #[test]
    fn finalize_turn_drops_whitespace_only_text_with_no_trace() {
        let mut entries = vec![entry(Message::user("still there?"), None)];

        let outcome =
            finalize_turn_state(&mut entries, None, "   \n".to_string(), Vec::new(), None);

        assert_eq!(
            outcome,
            TurnOutcome::DroppedAndRolledBack("still there?".to_string())
        );
        assert!(entries.is_empty());
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
            crate::testing::tool_name("read_file"),
            serde_json::json!({ "path": format!("{id}.md") }),
        ));
        Message::Assistant(rig_core::message::AssistantMessage::new(content))
    }

    fn tool_result(id: &str) -> Message {
        Message::tool_result(
            rig_core::message::CallId::from_wire(id),
            crate::testing::tool_name("read_file"),
            format!("contents of {id}"),
        )
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
                let Message::Assistant(rig_core::message::AssistantMessage { content, .. }) =
                    previous
                else {
                    panic!("tool result at index {i} does not follow an assistant message");
                };
                assert!(
                    content.iter().any(|ac| matches!(
                        ac,
                        AssistantContent::ToolCall(tc) if tc.id == result.call
                    )),
                    "tool result {} at index {i} does not follow its call",
                    result.call
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
                Message::Assistant(_) if crate::services::is_tool_call_message(m) => "call",
                Message::Assistant(_) => "text",
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

    /// Both frontends finalize a streamed turn through `finalize_turn`, not
    /// `finalize_response` — so the ordering tests above, which drive
    /// `finalize_response_state` directly, do not by themselves prove the
    /// shipping path persists anything. This pins that wiring: the record
    /// parked by `StreamChunk::TurnMessages` must survive `finalize_turn_state`.
    ///
    /// Without it, the delegation could be dropped in a later refactor and
    /// every test here would still pass while tool turns silently stopped
    /// being persisted — which is exactly how AGE-247 arrived broken in the
    /// merge that introduced this path.
    #[test]
    fn finalize_turn_persists_tool_round_trips_on_the_live_path() {
        let mut entries = vec![entry(Message::user("read both"), None)];

        let outcome = finalize_turn_state(
            &mut entries,
            Some(two_tool_turn()),
            "Let me look.\n\nBoth read.".to_string(),
            Vec::new(),
            None,
        );

        assert!(matches!(outcome, TurnOutcome::Persisted));

        let shape: Vec<&str> = messages(&entries)
            .iter()
            .map(|m| match m {
                Message::User { .. } if is_tool_result_message(m) => "result",
                Message::User { .. } => "user",
                Message::Assistant(_) if crate::services::is_tool_call_message(m) => "call",
                Message::Assistant(_) => "text",
                Message::System { .. } => "system",
            })
            .collect();
        assert_eq!(shape, ["user", "call", "result", "call", "result", "text"]);
        assert_pairs_intact(&entries);
    }

    /// A turn that produced neither text nor a trace is rolled back, and its
    /// tool messages go with it: a dropped turn must not persist half a turn
    /// (tool calls with no answer behind them).
    #[test]
    fn a_dropped_turn_persists_none_of_its_tool_messages() {
        let mut entries = vec![entry(Message::user("read both"), None)];

        let outcome = finalize_turn_state(
            &mut entries,
            Some(two_tool_turn()),
            String::new(),
            Vec::new(),
            None,
        );

        assert!(matches!(outcome, TurnOutcome::DroppedAndRolledBack(_)));
        assert!(
            entries.is_empty(),
            "the pending user entry is rolled back and no tool message is left behind"
        );
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

    /// The restore path is the compat parser's caller (AGE-369): a row whose
    /// assistant content was written before the `type` tag existed opens.
    #[test]
    fn a_legacy_untagged_row_deserializes_through_the_restore_path() {
        let legacy = include_str!("snapshots/legacy_untagged_history.json");

        let history = Conversation::deserialize_history(legacy).expect("a legacy row restores");

        assert_eq!(history.len(), 2);
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
        assert!(matches!(entries[1].message, Message::Assistant(_)));

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
        assert!(matches!(entries[1].message, Message::Assistant(_)));
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

    #[test]
    fn a_cloud_model_reads_as_a_name() {
        for (identifier, name) in [
            ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
            ("anthropic/claude-opus-5-5", "Claude Opus 5.5"),
            ("qwen3-27b", "Qwen3 27B"),
            ("moonshotai/kimi-k2.6", "Kimi K2.6"),
            ("Claude Sonnet 5.5", "Claude Sonnet 5.5"),
        ] {
            assert_eq!(super::readable_model_name(identifier), name, "{identifier}");
        }
    }
}
