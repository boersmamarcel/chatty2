//! The output contract of an [`AgentSession`](super::AgentSession) turn.
//!
//! One `SessionEvent` per observable thing the turn did, in the order it
//! happened. Every frontend binds to this and nothing else: chatty-tui's
//! `AppEvent` and chatty-gpui's `StreamManagerEvent` are adapters over it, and
//! a future transport would serialize it.
//!
//! # Derivation
//!
//! The variants are the intersection of what the two frontends' event enums
//! consumed before the session existed (AGE-194), not a superset: each maps
//! onto something both `AppEvent` and `StreamManagerEvent` already carried, or
//! that both frontends consumed out-of-band (`ApiCallUsage`, `TurnMessages`,
//! `Delegation`, `FollowUp`).
//!
//! # Ordering
//!
//! * `TurnStarted` is always first and `TurnEnded` is always emitted, exactly
//!   once, after every other event of the turn — including on the paths where
//!   the stream never opened or failed mid-way.
//! * `Error` and `Cancelled` precede `TurnEnded`; they say *why* the turn is
//!   ending, `TurnEnded` says that it has. A cancelled turn is therefore
//!   `Cancelled` then `TurnEnded`, mirroring the loop's `on_cancelled` /
//!   `on_stream_ended` pair rather than folding the two into one variant.
//! * `FollowUp` is the only event that may follow `TurnEnded`: the prompt the
//!   agent protocol or the loop guard wants sent as the *next* turn, once the
//!   frontend has finalized this one.

use rig_core::completion::Message;

use crate::models::clarification_store::ClarifyingQuestion;
use crate::models::token_usage::{ApiCallUsage, TokenUsage};
use crate::services::StreamError;
use crate::tools::invoke_agent_tool::InvokeAgentProgress;

/// One observable step of a turn. See the module docs for ordering.
///
/// Serializable so a turn can cross a process boundary: a delegated worker
/// reports its events to the broker, which turns them into A2A status and
/// artifact updates for the parent (AGE-196, ADR-0011).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum SessionEvent {
    /// The turn's stream loop is running.
    TurnStarted,
    /// A fragment of the assistant's text, in stream order.
    Text(String),
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
    /// A tool is waiting on the user; resolve through the session's
    /// approval store.
    ApprovalRequested {
        id: String,
        command: String,
        is_sandboxed: bool,
        /// What the approval is for, in the shape a worker relays it up the
        /// call chain in (AGE-646).
        detail: crate::models::execution_approval_store::ApprovalDetail,
    },
    ApprovalResolved {
        id: String,
        approved: bool,
    },
    /// `ask_user` is waiting on the user; answer through the session's
    /// clarification store.
    ClarificationRequested {
        id: String,
        questions: Vec<ClarifyingQuestion>,
    },
    /// Usage for one completed provider request within the turn.
    ApiCallUsage(ApiCallUsage),
    /// The turn's usage, folded from its per-request records (or the
    /// provider's aggregate when no per-request record arrived). Arrives
    /// before `TurnEnded`.
    TokenUsage(TokenUsage),
    /// What one of the agent's plugins spent through `llm::complete` during
    /// the turn (PL-U2, AGE-616): one line per plugin, named in
    /// [`TokenUsage::plugin`], each call carrying the model that served it.
    /// Arrives before `TurnEnded`; `finish_turn` records it beside the turn's
    /// own line, not in it, so the context fill stays the agent's own.
    PluginUsage(TokenUsage),
    /// rig's record of the turn's messages, for persisting the tool
    /// round-trips behind the final text (AGE-247).
    TurnMessages(Vec<Message>),
    /// Progress from a sub-agent the turn invoked.
    Delegation(InvokeAgentProgress),
    /// What a run nested under one of the turn's delegations did: a batch
    /// of one node's items, tagged by the broker with
    /// `(root_task_id, node, chain)` from its own task table, never by the
    /// worker (TB-1, AGE-663). At most one per node per
    /// [`FORWARD_INTERVAL`](chatty_fabric::FORWARD_INTERVAL); text arrives
    /// as its byte length only. It says what happened below the
    /// delegation for a view of the tree, which
    /// [`SwarmTrace`](crate::services::swarm_trace::SwarmTrace) builds
    /// (TB-2); the delegation's own progress
    /// and usage still arrive as [`Delegation`](Self::Delegation), which is
    /// what the turn records and bills.
    SwarmEvent(chatty_fabric::SwarmEvent),
    /// The stream ended in an error. `TurnEnded` still follows.
    Error(StreamError),
    /// The cancel flag was seen. `TurnEnded` still follows.
    Cancelled,
    /// The turn is over; the frontend finalizes it now.
    TurnEnded,
    /// A prompt to send as the next turn, after finalizing this one.
    FollowUp(FollowUp),
}

/// A follow-up the session asks for: the prompt, and the id it is known by
/// (ADR-0024 § 1). A hosted client sends back only the id — the server
/// injects the text it recorded under it — so a follow-up turn cannot
/// carry text the session never emitted.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FollowUp {
    pub id: String,
    pub prompt: String,
}

impl FollowUp {
    /// `prompt`, under a fresh id.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            prompt: prompt.into(),
        }
    }
}
