use crossterm::event::Event as CrosstermEvent;

/// Unified event type for the TUI application.
/// All async tasks (streaming, settings loading) send events through a single channel.
/// The main loop drains events between frames.
#[derive(Debug)]
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
    TitleGenerated(String),
    SubAgentFinished(String),

    // ── Terminal events ──────────────────────────────────────────────────
    TerminalInput(CrosstermEvent),
    Tick,
}
