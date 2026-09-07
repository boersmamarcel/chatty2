//! The session's [`StreamChunkHandler`]: maps each [`StreamChunk`] and
//! sub-agent progress event onto a [`SessionEvent`], and owns the per-turn
//! protocol state that used to be written once per frontend — the todo
//! protocol's follow-up, the loop guard, the malformed-tool-call retry and
//! the folding of per-request usage into the turn's record.
//!
//! Nothing here knows about a UI. The handler emits through a plain
//! `FnMut(SessionEvent)`, so the TUI hands it a channel sender and the desktop
//! a closure over its `AsyncApp`; the loop it runs under is
//! [`run_stream_loop`](crate::services::run_stream_loop), which places no
//! `Send` bound on it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::models::token_usage::{ApiCallUsage, TokenUsage};
use crate::services::llm_service::StreamChunk;
use crate::services::{
    AgentLoopGuard, AgentTaskController, ChunkAction, FollowUpReason, RecoveryAction,
    StreamChunkHandler, StreamError, StreamErrorKind, StreamSurface, decide_recovery,
    follow_up_requires_cancel,
};
use crate::tools::invoke_agent_tool::InvokeAgentProgress;

use super::SessionEvent;

/// Injected once when a provider rejects a tool call for malformed JSON.
///
/// The `Agent protocol follow-up:` prefix is what
/// [`is_protocol_follow_up_text`](crate::services::is_protocol_follow_up_text)
/// matches on, which keeps this hidden from the transcript like every other
/// injected nudge, and is what [`TurnPolicy::already_asked_to_retry`] is
/// computed from.
pub const MALFORMED_TOOL_CALL_FOLLOW_UP: &str = "Agent protocol follow-up: your last tool call was rejected because its JSON arguments \
     were malformed or truncated. Make the same call again, keeping the arguments small and \
     fully closed. If the arguments were large, write the content to a file in smaller steps \
     instead.";

/// Injected after a text-only response that ran past the loop guard's
/// verbosity limit.
pub const BREVITY_FOLLOW_UP: &str = "You produced a long response without any tool call. \
     If you have enough information, give your final answer now. \
     Otherwise, make a single focused tool call to get what you need.";

/// True for the tools that change the agent's todo snapshot.
pub fn is_agent_todo_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "write_todos" | "update_todo" | "verify_completion"
    )
}

/// Per-turn decisions the handler makes on the session's behalf. Built by
/// [`AgentSession`](super::AgentSession) from its config and history, so the
/// handler itself reads no settings and no globals.
#[derive(Clone, Debug)]
pub struct TurnPolicy {
    /// Which recovery table applies to a stream-ending error.
    pub surface: StreamSurface,
    /// `ExecutionSettingsModel::max_agent_turns`; sizes the loop guard.
    pub max_agent_turns: usize,
    /// Whether to run [`AgentLoopGuard`] over the turn's tool calls and text.
    /// The desktop does; the interactive TUI does not, and headless runs its
    /// own (AGE-196 decides whether that one folds in here).
    pub loop_guard: bool,
    /// Whether the last user message in history is already the
    /// malformed-tool-call retry, which bounds that retry to one attempt.
    pub already_asked_to_retry: bool,
}

/// Turn-orchestration logic shared by every frontend. See the module docs.
pub struct SessionStreamHandler<F: FnMut(SessionEvent)> {
    emit: F,
    task_controller: AgentTaskController,
    loop_guard: Option<AgentLoopGuard>,
    cancel_flag: Arc<AtomicBool>,
    surface: StreamSurface,
    already_asked_to_retry: bool,
    /// id → name and id → arguments for the tool calls in flight.
    pending_tool_names: HashMap<String, String>,
    pending_tool_args: HashMap<String, String>,
    /// One record per completed provider request; folded into the turn's
    /// `TokenUsage` when the aggregate arrives.
    calls: Vec<ApiCallUsage>,
    /// The prompt to emit as `FollowUp` once the turn has ended. First one
    /// queued wins, except that a loop-guard pivot always replaces it: the
    /// pivot cancels the turn, so whatever was queued before is moot.
    pending_follow_up: Option<String>,
    text_overflow: bool,
}

impl<F: FnMut(SessionEvent)> SessionStreamHandler<F> {
    pub fn new(
        emit: F,
        task_controller: AgentTaskController,
        cancel_flag: Arc<AtomicBool>,
        policy: TurnPolicy,
    ) -> Self {
        Self {
            emit,
            task_controller,
            loop_guard: policy
                .loop_guard
                .then(|| AgentLoopGuard::new(policy.max_agent_turns, false)),
            cancel_flag,
            surface: policy.surface,
            already_asked_to_retry: policy.already_asked_to_retry,
            pending_tool_names: HashMap::new(),
            pending_tool_args: HashMap::new(),
            calls: Vec::new(),
            pending_follow_up: None,
            text_overflow: false,
        }
    }

    /// Hand the emitter back once the loop is done, so a caller that gave the
    /// handler a closure over its own state can have that state back.
    pub fn into_emitter(self) -> F {
        self.emit
    }

    /// Queue `prompt` unless one is already queued this turn (AGE-242 / D3:
    /// the first follow-up wins, a later one is logged and dropped).
    fn queue_follow_up(&mut self, prompt: String) {
        if self.pending_follow_up.is_none() {
            self.pending_follow_up = Some(prompt);
        } else {
            tracing::warn!(
                dropped_prompt = %prompt,
                "Dropping a later follow-up; an earlier one is already queued"
            );
        }
    }

    /// The protocol's reaction to a finished tool call, success or error.
    ///
    /// The todo nudge never cancels the in-flight turn (shared policy,
    /// AGE-151): it waits for the turn to end naturally. A loop-guard pivot
    /// does cancel, since letting the turn run on is the thing it prevents.
    fn on_tool_completed(&mut self, id: &str) {
        let tool_name = self.pending_tool_names.remove(id).unwrap_or_default();
        let tool_args = self.pending_tool_args.remove(id).unwrap_or_default();

        if let Some(prompt) = self.task_controller.observe_tool_result(&tool_name) {
            tracing::debug!(
                "Agent todo protocol: multiple tool results observed before write_todos"
            );
            if follow_up_requires_cancel(FollowUpReason::TodoProtocol) {
                self.cancel_flag.store(true, Ordering::Relaxed);
            }
            self.queue_follow_up(prompt);
        }
        if let Some(guard) = self.loop_guard.as_mut()
            && let Some(pivot) = guard.on_tool_completed(&tool_name, &tool_args)
        {
            tracing::debug!(pivot = %pivot, "AgentLoopGuard loop detected; cancelling the turn");
            if follow_up_requires_cancel(FollowUpReason::LoopGuard) {
                self.cancel_flag.store(true, Ordering::Relaxed);
            }
            self.pending_follow_up = Some(pivot);
        }
    }

    /// Fold the turn's per-request records into its usage. The records are
    /// the source of truth (cache hit rate is per request); the provider's
    /// aggregate only stands in when none arrived.
    fn fold_usage(&mut self, aggregate: ApiCallUsage) -> TokenUsage {
        if self.calls.is_empty() {
            let mut usage = TokenUsage::new(aggregate.input_tokens, aggregate.output_tokens);
            usage.cache_read_tokens = aggregate.cache_read_tokens;
            usage.cache_write_tokens = aggregate.cache_write_tokens;
            return usage;
        }
        let usage = TokenUsage::from_calls(std::mem::take(&mut self.calls));
        if usage.input_tokens != aggregate.input_tokens
            || usage.output_tokens != aggregate.output_tokens
        {
            tracing::warn!(
                summed_input = usage.input_tokens,
                reported_input = aggregate.input_tokens,
                summed_output = usage.output_tokens,
                reported_output = aggregate.output_tokens,
                "Per-call usage does not sum to the provider's aggregate"
            );
        }
        usage
    }

    /// What to do about a stream-ending error, per the shared policy table
    /// (AGE-244 / D5). The only recovery a turn can carry out itself is the
    /// malformed-tool-call nudge; the rest is logged or left to the frontend.
    fn on_stream_error(&mut self, error: &StreamError) {
        tracing::error!(error = %error.message, kind = ?error.kind, "Stream error");
        match decide_recovery(error.kind, self.surface, 0) {
            RecoveryAction::Retry { .. } if error.kind == StreamErrorKind::Auth => {
                // The Entra token is attached fresh to every request
                // (AGE-245), so an auth rejection is a credential problem with
                // nothing left for the turn to refresh.
                tracing::warn!(
                    "Authentication rejected - check the configured API key/header, or the Entra ID credential (az login, managed identity or service principal)"
                );
            }
            // A transport retry is the surface's own loop (headless
            // `recovery.rs`), driven from the `Error` event after the turn.
            RecoveryAction::Retry { .. } => {}
            RecoveryAction::Nudge => {
                // The cap lives in conversation history, not in a field: the
                // handler is rebuilt for every injected follow-up, so a flag
                // here would reset each time and the retry would never
                // terminate (AGE-150 Defect 2).
                if self.pending_follow_up.is_none() && !self.already_asked_to_retry {
                    tracing::warn!(error = %error.message, "Malformed tool-call JSON; asking the model to retry");
                    self.pending_follow_up = Some(MALFORMED_TOOL_CALL_FOLLOW_UP.to_string());
                }
            }
            RecoveryAction::Stop => {}
        }
    }
}

impl<F: FnMut(SessionEvent)> StreamChunkHandler for SessionStreamHandler<F> {
    fn on_stream_started(&mut self) {
        (self.emit)(SessionEvent::TurnStarted);
    }

    fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            // The generic `ResponseStream` contract's transport `Err`. rig
            // errors always arrive typed as `StreamChunk::Error` (see
            // `map_stream_result` in llm_service), so this arm is reached by
            // test fixtures modelling a raw stream failure. It ends the turn
            // the same way a typed error does: with an `Error` event the
            // frontend can show, never by propagating out of the loop.
            Err(e) => {
                let error = StreamError::new(StreamErrorKind::Other, e.to_string());
                self.on_stream_error(&error);
                (self.emit)(SessionEvent::Error(error));
                return Ok(ChunkAction::Break);
            }
        };

        match chunk {
            StreamChunk::Text(text) => {
                if let Some(guard) = self.loop_guard.as_mut()
                    && !self.text_overflow
                    && guard.on_text_chunk(text.len())
                {
                    self.text_overflow = true;
                    tracing::debug!(
                        "Text-only response exceeded the verbosity limit; a brevity prompt follows the turn"
                    );
                }
                (self.emit)(SessionEvent::Text(text));
            }
            StreamChunk::ToolCallStarted { id, name } => {
                self.pending_tool_names.insert(id.clone(), name.clone());
                (self.emit)(SessionEvent::ToolCallStarted { id, name });
            }
            StreamChunk::ToolCallInput { id, arguments } => {
                self.pending_tool_args
                    .entry(id.clone())
                    .or_default()
                    .push_str(&arguments);
                (self.emit)(SessionEvent::ToolCallInput { id, arguments });
            }
            StreamChunk::ToolCallResult { id, result } => {
                // Forward first: the result is always delivered, whatever the
                // protocol decides to queue for after the turn.
                (self.emit)(SessionEvent::ToolCallResult {
                    id: id.clone(),
                    result,
                });
                self.on_tool_completed(&id);
            }
            StreamChunk::ToolCallError { id, error } => {
                (self.emit)(SessionEvent::ToolCallError {
                    id: id.clone(),
                    error,
                });
                self.on_tool_completed(&id);
            }
            StreamChunk::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => (self.emit)(SessionEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            }),
            StreamChunk::ApprovalResolved { id, approved } => {
                (self.emit)(SessionEvent::ApprovalResolved { id, approved })
            }
            StreamChunk::ClarificationRequested { id, questions } => {
                (self.emit)(SessionEvent::ClarificationRequested { id, questions })
            }
            StreamChunk::ApiCallUsage(call) => {
                self.calls.push(call);
                (self.emit)(SessionEvent::ApiCallUsage(call));
            }
            StreamChunk::TurnUsage(aggregate) => {
                let usage = self.fold_usage(aggregate);
                (self.emit)(SessionEvent::TokenUsage(usage));
            }
            StreamChunk::TurnMessages(messages) => {
                (self.emit)(SessionEvent::TurnMessages(messages));
            }
            StreamChunk::Done => {
                if self.text_overflow && self.pending_follow_up.is_none() {
                    self.pending_follow_up = Some(BREVITY_FOLLOW_UP.to_string());
                }
                if self.pending_follow_up.is_none() {
                    self.pending_follow_up = self.task_controller.stream_end_follow_up();
                }
                return Ok(ChunkAction::Break);
            }
            StreamChunk::Error(error) => {
                self.on_stream_error(&error);
                (self.emit)(SessionEvent::Error(error));
                return Ok(ChunkAction::Break);
            }
        }
        Ok(ChunkAction::Continue)
    }

    fn on_progress(&mut self, progress: InvokeAgentProgress) {
        (self.emit)(SessionEvent::SubAgent(progress));
    }

    fn on_cancelled(&mut self) {
        (self.emit)(SessionEvent::Cancelled);
    }

    fn on_stream_ended(&mut self) {
        (self.emit)(SessionEvent::TurnEnded);
        if let Some(prompt) = self.pending_follow_up.take() {
            (self.emit)(SessionEvent::FollowUp(prompt));
        }
    }
}
