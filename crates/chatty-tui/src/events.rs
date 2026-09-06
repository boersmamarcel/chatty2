use crossterm::event::Event as CrosstermEvent;

use chatty_core::models::Conversation;
use chatty_core::services::github_pr_service::PullRequestSummary;
use chatty_core::services::{EmbeddingService, McpService, MemoryService, StreamError};

/// Heavy services loaded in the background after the TUI is displayed.
/// Delivered via `AppEvent::ServicesReady` so the engine can patch itself.
pub struct DeferredServices {
    pub user_secrets: Vec<(String, String)>,
    pub mcp_service: Option<McpService>,
    pub memory_service: Option<MemoryService>,
    pub search_settings:
        Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<EmbeddingService>,
}

/// Unified event type for the TUI application.
/// All async tasks (streaming, settings loading) send events through a single channel.
/// The main loop drains events between frames.
#[allow(dead_code)] // Variants constructed by async tasks, matched in engine/app
pub enum AppEvent {
    // ── Stream events ────────────────────────────────────────────────────
    StreamStarted,
    TextChunk(String),
    ToolCallStarted {
        id: String,
        name: String,
    },
    ToolCallInput {
        id: String,
        arguments: String,
    },
    ToolCallResult {
        id: String,
        result: String,
    },
    ToolCallError {
        id: String,
        error: String,
    },
    ApprovalRequested {
        id: String,
        command: String,
        is_sandboxed: bool,
    },
    ApprovalResolved {
        id: String,
        approved: bool,
    },
    ClarificationRequested {
        id: String,
        questions: Vec<chatty_core::models::clarification_store::ClarifyingQuestion>,
    },
    /// Per-request usage for one provider call within the turn. Folded into
    /// `ChatEngine::last_turn_usage` once `TokenUsage` (the turn's aggregate)
    /// arrives, so the last call's prompt size can stand in for the actual
    /// current context fill (AGE-223).
    ApiCallUsage(chatty_core::models::token_usage::ApiCallUsage),
    /// The turn's usage, folded from its per-request records by the session
    /// (the last call's prompt size is the actual context fill, AGE-223).
    TokenUsage(chatty_core::models::token_usage::TokenUsage),
    /// rig's record of the turn's messages, persisted behind the final text
    /// when the stream completes (AGE-247).
    TurnMessages(Vec<rig_core::completion::Message>),
    StreamCompleted,
    StreamCancelled,
    StreamError(StreamError),
    AgentProtocolFollowUp(String),

    // ── Lifecycle events ─────────────────────────────────────────────────
    ConversationReady,
    /// Background conversation initialization completed successfully.
    ConversationInitialized {
        conversation: Box<Conversation>,
        generation: u64,
    },
    /// Background conversation initialization failed.
    ConversationInitFailed(String),
    /// Deferred services (MCP, memory, embedding, etc.) finished loading.
    ServicesReady(Box<DeferredServices>),
    /// Git branch detection completed in background.
    GitBranchDetected(Option<String>),
    /// GitHub pull request lookup for the workspace branch completed.
    PullRequestDetected(Option<Box<PullRequestSummary>>),
    TitleGenerated(String),
    SubAgentProgress(String),
    SubAgentFinished(String),

    // ── Terminal events ──────────────────────────────────────────────────
    TerminalInput(CrosstermEvent),
    Tick,
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StreamStarted => write!(f, "StreamStarted"),
            Self::TextChunk(s) => f.debug_tuple("TextChunk").field(s).finish(),
            Self::ToolCallStarted { id, name } => f
                .debug_struct("ToolCallStarted")
                .field("id", id)
                .field("name", name)
                .finish(),
            Self::ToolCallInput { id, arguments } => f
                .debug_struct("ToolCallInput")
                .field("id", id)
                .field("arguments", arguments)
                .finish(),
            Self::ToolCallResult { id, result } => f
                .debug_struct("ToolCallResult")
                .field("id", id)
                .field("result", result)
                .finish(),
            Self::ToolCallError { id, error } => f
                .debug_struct("ToolCallError")
                .field("id", id)
                .field("error", error)
                .finish(),
            Self::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => f
                .debug_struct("ApprovalRequested")
                .field("id", id)
                .field("command", command)
                .field("is_sandboxed", is_sandboxed)
                .finish(),
            Self::ApprovalResolved { id, approved } => f
                .debug_struct("ApprovalResolved")
                .field("id", id)
                .field("approved", approved)
                .finish(),
            Self::ClarificationRequested { id, questions } => f
                .debug_struct("ClarificationRequested")
                .field("id", id)
                .field("questions", &questions.len())
                .finish(),
            Self::ApiCallUsage(call) => f.debug_tuple("ApiCallUsage").field(call).finish(),
            Self::TokenUsage(usage) => f.debug_tuple("TokenUsage").field(usage).finish(),
            Self::TurnMessages(messages) => f
                .debug_tuple("TurnMessages")
                .field(&messages.len())
                .finish(),
            Self::StreamCompleted => write!(f, "StreamCompleted"),
            Self::StreamCancelled => write!(f, "StreamCancelled"),
            Self::StreamError(s) => f.debug_tuple("StreamError").field(s).finish(),
            Self::AgentProtocolFollowUp(s) => {
                f.debug_tuple("AgentProtocolFollowUp").field(s).finish()
            }
            Self::ConversationReady => write!(f, "ConversationReady"),
            Self::ConversationInitialized { generation, .. } => f
                .debug_struct("ConversationInitialized")
                .field("generation", generation)
                .finish_non_exhaustive(),
            Self::ConversationInitFailed(s) => {
                f.debug_tuple("ConversationInitFailed").field(s).finish()
            }
            Self::ServicesReady(_) => write!(f, "ServicesReady"),
            Self::GitBranchDetected(b) => f.debug_tuple("GitBranchDetected").field(b).finish(),
            Self::PullRequestDetected(pr) => {
                f.debug_tuple("PullRequestDetected").field(pr).finish()
            }
            Self::TitleGenerated(s) => f.debug_tuple("TitleGenerated").field(s).finish(),
            Self::SubAgentProgress(s) => f.debug_tuple("SubAgentProgress").field(s).finish(),
            Self::SubAgentFinished(s) => f.debug_tuple("SubAgentFinished").field(s).finish(),
            Self::TerminalInput(e) => f.debug_tuple("TerminalInput").field(e).finish(),
            Self::Tick => write!(f, "Tick"),
        }
    }
}

/// The TUI's binding to chatty-core's turn contract (AGE-194): every
/// [`SessionEvent`](chatty_core::session::SessionEvent) maps onto exactly
/// one stream `AppEvent`, so the engine's event handling is unchanged by
/// where the turn runs.
///
/// One deliberate reconciliation with the pre-session TUI: a transport
/// failure mid-stream arrives as `StreamError` *before* `StreamCompleted`,
/// rather than as an `Err` the spawning task turned into a `StreamError`
/// *after* it. The sequence is the desktop's, and the one a frontend can act
/// on in order.
impl From<chatty_core::session::SessionEvent> for AppEvent {
    fn from(event: chatty_core::session::SessionEvent) -> Self {
        use chatty_core::session::SessionEvent;
        use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;

        match event {
            SessionEvent::TurnStarted => AppEvent::StreamStarted,
            SessionEvent::Text(text) => AppEvent::TextChunk(text),
            SessionEvent::ToolCallStarted { id, name } => AppEvent::ToolCallStarted { id, name },
            SessionEvent::ToolCallInput { id, arguments } => {
                AppEvent::ToolCallInput { id, arguments }
            }
            SessionEvent::ToolCallResult { id, result } => AppEvent::ToolCallResult { id, result },
            SessionEvent::ToolCallError { id, error } => AppEvent::ToolCallError { id, error },
            SessionEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => AppEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            },
            SessionEvent::ApprovalResolved { id, approved } => {
                AppEvent::ApprovalResolved { id, approved }
            }
            SessionEvent::ClarificationRequested { id, questions } => {
                AppEvent::ClarificationRequested { id, questions }
            }
            SessionEvent::ApiCallUsage(call) => AppEvent::ApiCallUsage(call),
            SessionEvent::TokenUsage(usage) => AppEvent::TokenUsage(usage),
            SessionEvent::TurnMessages(messages) => AppEvent::TurnMessages(messages),
            SessionEvent::SubAgent(progress) => match progress {
                InvokeAgentProgress::Started {
                    agent_name,
                    prompt,
                    source,
                } => {
                    let mode = match source {
                        chatty_core::models::message_types::ToolSource::Local => "local",
                        _ => "remote",
                    };
                    AppEvent::SubAgentProgress(format!("[{mode} agent: {agent_name}] {prompt}"))
                }
                InvokeAgentProgress::Text(text) => AppEvent::SubAgentProgress(text),
                InvokeAgentProgress::Finished { success, result } => {
                    AppEvent::SubAgentFinished(result.unwrap_or_else(|| {
                        if success {
                            "Agent completed.".to_string()
                        } else {
                            "Agent failed.".to_string()
                        }
                    }))
                }
            },
            SessionEvent::Error(error) => AppEvent::StreamError(error),
            SessionEvent::Cancelled => AppEvent::StreamCancelled,
            SessionEvent::TurnEnded => AppEvent::StreamCompleted,
            SessionEvent::FollowUp(prompt) => AppEvent::AgentProtocolFollowUp(prompt),
        }
    }
}
