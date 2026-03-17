use crossterm::event::Event as CrosstermEvent;

use chatty_core::models::Conversation;

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
    TokenUsage {
        input_tokens: u32,
        output_tokens: u32,
    },
    StreamCompleted,
    StreamCancelled,
    StreamError(String),

    // ── Lifecycle events ─────────────────────────────────────────────────
    ConversationReady,
    /// Background conversation initialization completed successfully.
    ConversationInitialized {
        conversation: Box<Conversation>,
        generation: u64,
    },
    /// Background conversation initialization failed.
    ConversationInitFailed(String),
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
            Self::TokenUsage {
                input_tokens,
                output_tokens,
            } => f
                .debug_struct("TokenUsage")
                .field("input_tokens", input_tokens)
                .field("output_tokens", output_tokens)
                .finish(),
            Self::StreamCompleted => write!(f, "StreamCompleted"),
            Self::StreamCancelled => write!(f, "StreamCancelled"),
            Self::StreamError(s) => f.debug_tuple("StreamError").field(s).finish(),
            Self::ConversationReady => write!(f, "ConversationReady"),
            Self::ConversationInitialized { generation, .. } => f
                .debug_struct("ConversationInitialized")
                .field("generation", generation)
                .finish_non_exhaustive(),
            Self::ConversationInitFailed(s) => {
                f.debug_tuple("ConversationInitFailed").field(s).finish()
            }
            Self::TitleGenerated(s) => f.debug_tuple("TitleGenerated").field(s).finish(),
            Self::SubAgentProgress(s) => f.debug_tuple("SubAgentProgress").field(s).finish(),
            Self::SubAgentFinished(s) => f.debug_tuple("SubAgentFinished").field(s).finish(),
            Self::TerminalInput(e) => f.debug_tuple("TerminalInput").field(e).finish(),
            Self::Tick => write!(f, "Tick"),
        }
    }
}
