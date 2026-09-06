//! `AgentSession`: one conversation's turn lifecycle, UI-agnostic (AGE-194).
//!
//! Both frontends used to write the same turn twice — approval channels, then
//! context shaping, then `stream_prompt`, then the chunk loop, then the
//! finalize — once in chatty-tui's `ChatEngine` and once in chatty-gpui's
//! `run_llm_stream`. This module is that turn, written once. A frontend owns
//! an `AgentSession`, asks it to [`begin_turn`](AgentSession::begin_turn),
//! spawns the future it gets back on whatever executor it has, and receives
//! the turn as a stream of [`SessionEvent`]s. Its own event enum is an
//! adapter over that: `From<SessionEvent> for AppEvent` in the TUI,
//! `StreamManager::handle_session_event` on the desktop.
//!
//! # Ownership
//!
//! The session owns the [`Conversation`] and the three per-agent stores
//! (execution approvals, write approvals, clarifications) whose handles the
//! agent's tools were built with, so a request raised by a tool reaches this
//! session's receivers and no other's. Two sessions in one process share
//! nothing: there is no `'static` state here, and settings arrive by value in
//! [`AgentSessionConfig`] — the session never calls a repository accessor.
//!
//! # A turn, end to end
//!
//! 1. [`begin_turn`](AgentSession::begin_turn) snapshots history, commits the
//!    user message, installs fresh approval/clarification channels on the
//!    stores, arms a cancel flag, and returns the turn as a future.
//! 2. The future shapes the context, opens the stream, and drives
//!    [`run_stream_loop`](crate::services::run_stream_loop) with the
//!    session's [`SessionStreamHandler`], which emits every event.
//! 3. The owner feeds each event to [`apply`](AgentSession::apply), which
//!    folds the UI-agnostic part into the conversation (streaming text, the
//!    turn's messages, the todo snapshot, usage), then does its own display
//!    work.
//! 4. On `TurnEnded` the owner calls [`finish_turn`](AgentSession::finish_turn)
//!    with the trace and artifacts only it can provide; the session commits
//!    the turn under the shared empty-turn rule (AGE-243 / D4) and clears its
//!    per-turn state. A `FollowUp` after that is the next turn's input.

mod event;
mod handler;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use rig_core::completion::Message;
use rig_core::message::UserContent;

use crate::factories::AgentClient;
use crate::factories::agent_factory::{AgentBuildContext, BuiltAgent};
use crate::models::clarification_store::{
    ClarificationStore, ClarifyingQuestion, PendingClarifications,
};
use crate::models::conversation::{Conversation, TurnOutcome};
use crate::models::execution_approval_store::{ExecutionApprovalStore, PendingApprovals};
use crate::models::message_types::{
    ApprovalBlock, ApprovalState, ClarificationBlock, ClarificationState, ToolCallBlock,
    ToolCallState, classify_initial_execution_engine, classify_tool_source,
    detect_execution_engine, friendly_tool_name, is_denial_result, predict_execution_engine,
};
use crate::models::token_usage::TokenUsage;
use crate::models::write_approval_store::{PendingWriteApprovals, WriteApprovalStore};
use crate::repositories::ConversationData;
use crate::services::{
    AgentTaskController, AgentTaskSnapshot, ContextShaperSettings, RecoveryAction, StreamError,
    StreamErrorKind, StreamSurface, decide_recovery, exchange_count, extract_user_text,
    install_progress_channel, run_stream_loop, shape_context, stream_prompt,
};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;
use crate::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};

pub use event::SessionEvent;
pub use handler::{
    BREVITY_FOLLOW_UP, MALFORMED_TOOL_CALL_FOLLOW_UP, SessionStreamHandler, TurnPolicy,
    is_agent_todo_tool,
};

/// Everything a session needs from settings, by value. Loading is the
/// frontend's job; the session never reaches for a repository.
#[derive(Clone)]
pub struct AgentSessionConfig {
    pub execution_settings: ExecutionSettingsModel,
    /// Which recovery table applies to a stream-ending error (AGE-244 / D5).
    pub surface: StreamSurface,
    /// Whether turns run under [`AgentLoopGuard`](crate::services::AgentLoopGuard).
    pub loop_guard: bool,
}

/// The per-agent store handles an [`AgentBuildContext`](crate::factories::AgentBuildContext)
/// needs, so the agent's tools raise their requests on this session's stores.
#[derive(Clone)]
pub struct ApprovalHandles {
    pub pending_approvals: PendingApprovals,
    pub pending_write_approvals: PendingWriteApprovals,
    pub pending_clarifications: PendingClarifications,
}

/// Why a turn is being sent; decides what the session commits and resets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnKind {
    /// A human typed it: persisted as a user message, and the todo protocol
    /// starts from a clean state (AGE-150).
    Human,
    /// An injected agent-protocol / loop-guard follow-up: persisted so the
    /// model sees it, but the todo state of the turn it belongs to is kept.
    ProtocolFollowUp,
    /// Re-running the user message at the tail of the conversation's
    /// history: it is sent again, nothing is added, and the todo protocol
    /// starts clean like a human turn.
    Regenerate,
}

/// What to send.
pub struct TurnInput {
    /// The user message: persisted and sent. Ignored for
    /// [`TurnKind::Regenerate`], which takes the history's last user
    /// message instead.
    pub contents: Vec<UserContent>,
    /// Sent alongside `contents` but never persisted — e.g. the previous
    /// turn's assistant-generated attachments (AGE-216).
    pub llm_only_contents: Vec<UserContent>,
    /// Attachment paths persisted with the user message.
    pub attachments: Vec<PathBuf>,
    pub kind: TurnKind,
}

impl TurnInput {
    /// A plain human text message.
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            contents: vec![UserContent::text(message.into())],
            llm_only_contents: Vec::new(),
            attachments: Vec::new(),
            kind: TurnKind::Human,
        }
    }

    /// An injected follow-up (see [`TurnKind::ProtocolFollowUp`]).
    pub fn protocol_follow_up(prompt: impl Into<String>) -> Self {
        Self {
            kind: TurnKind::ProtocolFollowUp,
            ..Self::text(prompt)
        }
    }

    /// Re-run the last user message (see [`TurnKind::Regenerate`]).
    pub fn regenerate() -> Self {
        Self {
            contents: Vec::new(),
            llm_only_contents: Vec::new(),
            attachments: Vec::new(),
            kind: TurnKind::Regenerate,
        }
    }
}

/// One conversation's turn lifecycle. See the module docs.
pub struct AgentSession {
    config: AgentSessionConfig,
    conversation: Option<Conversation>,
    execution_approvals: ExecutionApprovalStore,
    write_approvals: WriteApprovalStore,
    clarifications: ClarificationStore,
    /// Armed for the duration of a turn; `None` between turns.
    cancel_flag: Option<Arc<AtomicBool>>,
    /// id → name for the tool calls in flight, so `apply` can tell a todo
    /// tool's result from any other's.
    pending_tool_names: HashMap<String, String>,
    /// Usage of the most recent turn that reported any.
    last_turn_usage: Option<TokenUsage>,
    /// Stream-error recovery attempts per kind across the turns of one task;
    /// reset by a human turn (AGE-273).
    recovery_attempts: HashMap<StreamErrorKind, usize>,
}

impl AgentSession {
    /// A session with its own stores and no conversation yet: build the agent
    /// against [`approval_handles`](Self::approval_handles), then
    /// [`set_conversation`](Self::set_conversation).
    pub fn new(config: AgentSessionConfig) -> Self {
        Self {
            config,
            conversation: None,
            execution_approvals: ExecutionApprovalStore::new(),
            write_approvals: WriteApprovalStore::new(),
            clarifications: ClarificationStore::new(),
            cancel_flag: None,
            pending_tool_names: HashMap::new(),
            last_turn_usage: None,
            recovery_attempts: HashMap::new(),
        }
    }

    pub fn config(&self) -> &AgentSessionConfig {
        &self.config
    }

    /// Replace the settings for later turns (the agent is the caller's to
    /// rebuild; tools carry their own copy from their build context).
    pub fn set_config(&mut self, config: AgentSessionConfig) {
        self.config = config;
    }

    pub fn approval_handles(&self) -> ApprovalHandles {
        ApprovalHandles {
            pending_approvals: self.execution_approvals.get_pending_approvals(),
            pending_write_approvals: self.write_approvals.get_pending_approvals(),
            pending_clarifications: self.clarifications.get_pending_clarifications(),
        }
    }

    pub fn execution_approvals(&self) -> &ExecutionApprovalStore {
        &self.execution_approvals
    }

    pub fn write_approvals(&self) -> &WriteApprovalStore {
        &self.write_approvals
    }

    pub fn clarifications(&self) -> &ClarificationStore {
        &self.clarifications
    }

    pub fn conversation(&self) -> Option<&Conversation> {
        self.conversation.as_ref()
    }

    pub fn conversation_mut(&mut self) -> Option<&mut Conversation> {
        self.conversation.as_mut()
    }

    pub fn set_conversation(&mut self, conversation: Option<Conversation>) {
        self.conversation = conversation;
    }

    pub fn take_conversation(&mut self) -> Option<Conversation> {
        self.conversation.take()
    }

    /// Complete a build context with what the session provides: its store
    /// handles, so the agent's tools raise requests on this session, and the
    /// owned conversation's artifact queue. The caller assembles only the
    /// services part (AGE-272).
    pub fn build_context(&self, mut ctx: AgentBuildContext) -> AgentBuildContext {
        let handles = self.approval_handles();
        ctx.pending_approvals = Some(handles.pending_approvals);
        ctx.pending_write_approvals = Some(handles.pending_write_approvals);
        ctx.pending_clarifications = Some(handles.pending_clarifications);
        if let Some(conversation) = &self.conversation {
            ctx.pending_artifacts = Some(conversation.pending_artifacts());
        }
        ctx
    }

    /// Build a new conversation's agent against this session's stores and
    /// own the conversation.
    pub async fn create_conversation(
        &mut self,
        id: String,
        title: String,
        model: &ModelConfig,
        provider: &ProviderConfig,
        ctx: AgentBuildContext,
    ) -> Result<()> {
        let ctx = self.build_context(ctx);
        let conversation = Conversation::new(id, title, model, provider, ctx).await?;
        self.conversation = Some(conversation);
        Ok(())
    }

    /// Restore a persisted conversation, building its agent against this
    /// session's stores, and own it.
    pub async fn restore_conversation(
        &mut self,
        data: ConversationData,
        model: &ModelConfig,
        provider: &ProviderConfig,
        ctx: AgentBuildContext,
    ) -> Result<()> {
        let ctx = self.build_context(ctx);
        let conversation = Conversation::from_data(data, model, provider, ctx).await?;
        self.conversation = Some(conversation);
        Ok(())
    }

    /// Install a rebuilt agent on the owned conversation: the client, the
    /// shell session the factory reused or created, and the progress slot.
    /// For owners that cannot hold the session across the build (the desktop
    /// builds inside a global). Returns false when there is no conversation.
    pub fn install_agent(
        &mut self,
        built: BuiltAgent,
        model_id: String,
        workspace_dir: Option<PathBuf>,
    ) -> bool {
        let Some(conversation) = self.conversation.as_mut() else {
            return false;
        };
        conversation.set_agent(Arc::new(built.client), model_id, workspace_dir);
        if built.shell_session.is_some() {
            conversation.set_shell_session(built.shell_session);
        }
        conversation.set_invoke_agent_progress_slot(built.invoke_agent_progress_slot);
        true
    }

    pub fn is_turn_active(&self) -> bool {
        self.cancel_flag.is_some()
    }

    /// Ask the running turn to stop. The loop sees the flag on its next pass
    /// and ends with `Cancelled` then `TurnEnded`. A no-op between turns.
    pub fn cancel(&self) {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, Ordering::Relaxed);
        }
    }

    pub fn last_turn_usage(&self) -> Option<&TokenUsage> {
        self.last_turn_usage.as_ref()
    }

    /// What to do about a stream-ending error, from the shared policy table
    /// (AGE-244 / D5) for this session's surface, counting the attempts
    /// already made for that kind of error since the last human turn. A
    /// `Retry` carries the delay the surface waits before sending its
    /// recovery prompt; a `Nudge` was already queued by the turn's handler.
    pub fn recovery_action(&mut self, error: &StreamError) -> RecoveryAction {
        let attempt = self.recovery_attempts.entry(error.kind).or_default();
        let action = decide_recovery(error.kind, self.config.surface, *attempt);
        if !matches!(action, RecoveryAction::Stop) {
            *attempt += 1;
        }
        action
    }

    /// Whether the conversation is ready for its generated title: still
    /// untitled after exactly one exchange. Counted on persisted history, so
    /// a turn's tool round-trips don't make one exchange look like several
    /// (AGE-247).
    pub fn should_generate_title(&self) -> bool {
        self.conversation.as_ref().is_some_and(|conv| {
            conv.title() == "New Chat"
                && exchange_count(conv.entries().iter().map(|e| &e.message)) == 1
        })
    }

    /// Start a turn. Returns the turn as a future for the caller to spawn;
    /// every outcome, including a stream that never opens, arrives through
    /// `emit` (see [`SessionEvent`] for the ordering guarantees).
    ///
    /// Fails without side effects when a turn is already running or there is
    /// no conversation.
    pub fn begin_turn<F: FnMut(SessionEvent)>(
        &mut self,
        input: TurnInput,
        emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        self.begin_turn_with_flag(input, Arc::new(AtomicBool::new(false)), emit)
    }

    /// [`begin_turn`](Self::begin_turn) with the owner's own cancellation
    /// token, for an owner that registers the turn with something that
    /// holds the token before the turn can start (the desktop's
    /// `StreamManager`). [`cancel`](Self::cancel) and the token are the
    /// same switch.
    pub fn begin_turn_with_flag<F: FnMut(SessionEvent)>(
        &mut self,
        input: TurnInput,
        cancel_flag: Arc<AtomicBool>,
        emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        let turn = self.prepare_turn(input, cancel_flag)?;
        Ok(turn.run(emit))
    }

    /// Everything `begin_turn` does before the stream is opened, kept apart
    /// so a test can run the same preparation against a scripted stream.
    fn prepare_turn(
        &mut self,
        input: TurnInput,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<PreparedTurn> {
        if self.is_turn_active() {
            bail!("a turn is already running");
        }
        let Some(conversation) = self.conversation.as_mut() else {
            bail!("no conversation");
        };

        let TurnInput {
            contents,
            llm_only_contents,
            attachments,
            kind,
        } = input;

        // Snapshot BEFORE committing the new message: `stream_prompt` appends
        // `contents` after the history it is given, with no de-duplication
        // (AGE-221). A regenerate sends the tail user message again, so it
        // comes off the snapshot and becomes the contents.
        let mut history = conversation.messages();
        let contents = if kind == TurnKind::Regenerate {
            match history.pop() {
                Some(Message::User { content }) => content,
                _ => bail!("regenerate needs a user message at the tail of history"),
            }
        } else {
            contents
        };
        // The retry is bounded by the message being sent, not the snapshot:
        // the nudge turn carries the follow-up text itself, and the handler
        // must not nudge a second time on it.
        let already_asked_to_retry = extract_user_text(&contents)
            .trim_start()
            .starts_with(MALFORMED_TOOL_CALL_FOLLOW_UP);

        let mut llm_contents = contents.clone();
        llm_contents.extend(llm_only_contents);
        if kind != TurnKind::Regenerate {
            conversation.add_user_message_with_attachments(
                Message::User { content: contents },
                attachments,
            );
        }

        let agent = conversation.agent();
        let task_controller = agent.task_controller();
        // A human turn starts from a clean todo protocol state: the controller
        // lives on the conversation's agent, so leftover state would otherwise
        // nudge forever and block a second write_todos (AGE-150). The
        // recovery budget is per task the same way.
        if kind != TurnKind::ProtocolFollowUp {
            task_controller.reset();
            self.recovery_attempts.clear();
        }

        // Fresh channels per turn, installed on the stores the tools hold
        // (AGE-246 / D7). Write approvals share the execution channel and UI.
        let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
        let (resolution_tx, resolution_rx) = tokio::sync::mpsc::unbounded_channel();
        let (clarification_tx, clarification_rx) = tokio::sync::mpsc::unbounded_channel();
        self.write_approvals.set_notifier(approval_tx.clone());
        self.execution_approvals
            .set_notifiers(approval_tx, resolution_tx);
        self.clarifications.set_notifier(clarification_tx);

        self.cancel_flag = Some(cancel_flag.clone());
        self.pending_tool_names.clear();

        Ok(PreparedTurn {
            agent,
            task_controller,
            history,
            contents: llm_contents,
            approval_rx,
            resolution_rx,
            clarification_rx,
            cancel_flag,
            progress_slot: conversation.invoke_agent_progress_slot(),
            policy: TurnPolicy {
                surface: self.config.surface,
                max_agent_turns: self.config.execution_settings.max_agent_turns as usize,
                loop_guard: self.config.loop_guard,
                already_asked_to_retry,
            },
        })
    }

    /// Fold the UI-agnostic part of an event into the session: streaming
    /// text, the turn's trace (tool calls, approvals, clarifications,
    /// sub-agent progress), the turn's messages, the todo snapshot after a
    /// todo tool, and usage. Display state is the caller's, after this. An
    /// owner whose channel already carries its own event type calls the
    /// narrower methods below instead.
    ///
    /// Returns the agent's todo snapshot when this event changed it, for the
    /// owner's plan UI.
    pub fn apply(&mut self, event: &SessionEvent) -> Option<AgentTaskSnapshot> {
        match event {
            SessionEvent::Text(text) => self.append_streaming_text(text),
            SessionEvent::ToolCallStarted { id, name } => self.note_tool_started(id, name),
            SessionEvent::ToolCallInput { id, arguments } => self.note_tool_input(id, arguments),
            SessionEvent::ToolCallResult { id, result } => {
                return self.note_tool_result(id, result);
            }
            SessionEvent::ToolCallError { id, error } => return self.note_tool_error(id, error),
            SessionEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => self.note_approval_requested(id, command, *is_sandboxed),
            SessionEvent::ApprovalResolved { id, approved } => {
                self.note_approval_resolved(id, *approved)
            }
            SessionEvent::ClarificationRequested { id, questions } => {
                self.note_clarification_requested(id, questions)
            }
            SessionEvent::SubAgent(progress) => self.note_sub_agent(progress),
            SessionEvent::TurnMessages(messages) => self.set_turn_messages(messages.clone()),
            SessionEvent::TokenUsage(usage) => self.record_turn_usage(usage.clone()),
            _ => {}
        }
        None
    }

    /// `SessionEvent::Text`: extend the reply in flight.
    pub fn append_streaming_text(&mut self, text: &str) {
        if let Some(conversation) = self.conversation.as_mut() {
            conversation.append_streaming_content(text);
        }
    }

    /// `SessionEvent::ToolCallStarted`: open the tool call in the turn's
    /// trace and remember its name, so its result can be told apart from any
    /// other's.
    pub fn note_tool_started(&mut self, id: &str, name: &str) {
        self.pending_tool_names
            .insert(id.to_string(), name.to_string());
        if let Some(conversation) = self.conversation.as_mut() {
            let text_before = conversation
                .streaming_message()
                .cloned()
                .unwrap_or_default();
            let tool_call = ToolCallBlock {
                id: id.to_string(),
                tool_name: name.to_string(),
                display_name: friendly_tool_name(name),
                input: String::new(),
                output: None,
                output_preview: None,
                state: ToolCallState::Running,
                duration: None,
                text_before,
                source: classify_tool_source(name),
                execution_engine: classify_initial_execution_engine(name),
            };
            let trace = conversation.ensure_streaming_trace();
            let index = trace.items.len();
            trace.add_tool_call(tool_call);
            trace.set_active_tool(index);
        }
    }

    /// `SessionEvent::ToolCallInput`: the call's arguments, in the trace.
    pub fn note_tool_input(&mut self, id: &str, arguments: &str) {
        if let Some(trace) = self
            .conversation
            .as_mut()
            .and_then(|c| c.streaming_trace_mut())
        {
            trace.update_tool_call(id, |tc| {
                tc.execution_engine =
                    predict_execution_engine(&tc.tool_name, arguments).or(tc.execution_engine);
                tc.input = arguments.to_string();
            });
        }
    }

    /// `SessionEvent::ToolCallResult`: close the call in the trace and, after
    /// a todo tool, take the agent's new todo snapshot onto the conversation
    /// and return it for the owner's plan UI.
    pub fn note_tool_result(&mut self, id: &str, result: &str) -> Option<AgentTaskSnapshot> {
        if let Some(trace) = self
            .conversation
            .as_mut()
            .and_then(|c| c.streaming_trace_mut())
        {
            let denied = is_denial_result(result);
            trace.update_tool_call(id, |tc| {
                tc.execution_engine = detect_execution_engine(&tc.tool_name, result);
                tc.output = Some(result.to_string());
                tc.state = if denied {
                    ToolCallState::Error("Denied by user".to_string())
                } else {
                    ToolCallState::Success
                };
            });
            trace.clear_active_tool();
        }
        self.note_tool_finished(id)
    }

    /// `SessionEvent::ToolCallError`: close the call as failed in the trace;
    /// otherwise as [`note_tool_result`](Self::note_tool_result).
    pub fn note_tool_error(&mut self, id: &str, error: &str) -> Option<AgentTaskSnapshot> {
        if let Some(trace) = self
            .conversation
            .as_mut()
            .and_then(|c| c.streaming_trace_mut())
        {
            trace.update_tool_call(id, |tc| {
                tc.state = ToolCallState::Error(error.to_string());
            });
            trace.clear_active_tool();
        }
        self.note_tool_finished(id)
    }

    fn note_tool_finished(&mut self, id: &str) -> Option<AgentTaskSnapshot> {
        let name = self.pending_tool_names.remove(id)?;
        let conversation = self.conversation.as_mut()?;
        if !is_agent_todo_tool(&name) {
            return None;
        }
        let snapshot = conversation.agent().task_controller().snapshot();
        conversation.set_agent_task_snapshot(Some(snapshot.clone()));
        Some(snapshot)
    }

    /// `SessionEvent::ApprovalRequested`: the pending approval, in the trace.
    pub fn note_approval_requested(&mut self, id: &str, command: &str, is_sandboxed: bool) {
        if let Some(conversation) = self.conversation.as_mut() {
            let trace = conversation.ensure_streaming_trace();
            let index = trace.items.len();
            trace.add_approval(ApprovalBlock {
                id: id.to_string(),
                command: command.to_string(),
                is_sandboxed,
                state: ApprovalState::Pending,
                created_at: std::time::SystemTime::now(),
            });
            trace.set_active_tool(index);
        }
    }

    /// `SessionEvent::ApprovalResolved`: the decision, in the trace.
    pub fn note_approval_resolved(&mut self, id: &str, approved: bool) {
        if let Some(trace) = self
            .conversation
            .as_mut()
            .and_then(|c| c.streaming_trace_mut())
        {
            trace.update_approval_state(
                id,
                if approved {
                    ApprovalState::Approved
                } else {
                    ApprovalState::Denied
                },
            );
            trace.clear_active_tool();
        }
    }

    /// `SessionEvent::ClarificationRequested`: the questions, in the trace.
    /// The answers are the owner's to record, since it collects them.
    pub fn note_clarification_requested(&mut self, id: &str, questions: &[ClarifyingQuestion]) {
        if let Some(conversation) = self.conversation.as_mut() {
            let trace = conversation.ensure_streaming_trace();
            let index = trace.items.len();
            trace.add_clarification(ClarificationBlock {
                id: id.to_string(),
                questions: questions.to_vec(),
                answers: Vec::new(),
                state: ClarificationState::Pending,
                created_at: std::time::SystemTime::now(),
            });
            trace.set_active_tool(index);
        }
    }

    /// `SessionEvent::SubAgent`: the sub-agent's row on the conversation.
    pub fn note_sub_agent(&mut self, progress: &InvokeAgentProgress) {
        let Some(conversation) = self.conversation.as_mut() else {
            return;
        };
        match progress {
            InvokeAgentProgress::Started {
                agent_name,
                prompt,
                source,
            } => {
                conversation.start_sub_agent_progress(
                    &format!("[Agent: {agent_name}] {prompt}"),
                    source.clone(),
                );
            }
            InvokeAgentProgress::Text(text) => conversation.append_sub_agent_progress(text),
            InvokeAgentProgress::Finished { success, result } => {
                conversation.finalize_sub_agent_progress(*success, result.clone());
            }
        }
    }

    /// `SessionEvent::TurnMessages`: keep rig's record of the turn until
    /// `finish_turn` persists it (AGE-247).
    pub fn set_turn_messages(&mut self, messages: Vec<Message>) {
        if let Some(conversation) = self.conversation.as_mut() {
            conversation.set_streaming_turn_messages(Some(messages));
        }
    }

    /// `SessionEvent::TokenUsage`: the turn's usage, recorded on the
    /// conversation by `finish_turn`.
    pub fn record_turn_usage(&mut self, usage: TokenUsage) {
        self.last_turn_usage = Some(usage);
    }

    /// The turn's trace as the session recorded it, for an owner that wants
    /// to render or persist it itself. `None` until something is in it.
    pub fn trace_json(&self) -> Option<serde_json::Value> {
        self.conversation
            .as_ref()
            .and_then(|c| c.streaming_trace())
            .filter(|trace| trace.has_items())
            .and_then(|trace| serde_json::to_value(trace).ok())
    }

    /// Commit the turn under the shared empty-turn rule (AGE-243 / D4) and
    /// clear per-turn state. `artifacts` are the files `add_attachment`
    /// queued. `trace` is an owner-rendered trace to persist instead of the
    /// session's own (the desktop's carries the user's clarification
    /// answers); `None` persists the trace the session recorded from the
    /// turn's events, when it has anything in it (AGE-274).
    ///
    /// Returns `None` when there is no conversation or no turn to finish: a
    /// second call for the same turn is a no-op, so an owner that finalizes
    /// on both `Cancelled` and `TurnEnded` commits once. A
    /// `DroppedAndRolledBack` outcome carries the text of the user message
    /// that was rolled back, for the caller to put back into its composer.
    pub fn finish_turn(
        &mut self,
        trace: Option<serde_json::Value>,
        artifacts: Vec<PathBuf>,
    ) -> Option<TurnOutcome> {
        if !self.is_turn_active() {
            return None;
        }
        self.cancel_flag = None;
        self.pending_tool_names.clear();
        // Unblock any `ask_user` still waiting, so a cancelled turn cannot
        // leave a tool parked until its timeout.
        self.clarifications.cancel_all();

        let trace = trace.or_else(|| self.trace_json());
        let conversation = self.conversation.as_mut()?;
        let response = conversation
            .streaming_message()
            .cloned()
            .unwrap_or_default();
        let outcome = conversation.finalize_turn(response, artifacts, trace);
        conversation.set_streaming_message(None);
        conversation.set_streaming_trace(None);
        conversation.set_streaming_sub_agent_trace(None);

        if let Some(usage) = self.last_turn_usage.take() {
            conversation.add_token_usage(usage);
        }

        // If the follow-up budget ran out with verification still pending,
        // record it so the plan can say verification was skipped instead of
        // silently freezing on the last todo.
        let snapshot = conversation.agent().task_controller().snapshot();
        if snapshot.verification_skipped {
            conversation.set_agent_task_snapshot(Some(snapshot));
        }

        Some(outcome)
    }
}

/// A turn after [`AgentSession::prepare_turn`]: everything the async part
/// needs, owned, so the future borrows nothing from the session.
struct PreparedTurn {
    agent: Arc<AgentClient>,
    task_controller: AgentTaskController,
    history: Vec<Message>,
    contents: Vec<UserContent>,
    approval_rx: tokio::sync::mpsc::UnboundedReceiver<
        crate::models::execution_approval_store::ApprovalNotification,
    >,
    resolution_rx: tokio::sync::mpsc::UnboundedReceiver<
        crate::models::execution_approval_store::ApprovalResolution,
    >,
    clarification_rx: tokio::sync::mpsc::UnboundedReceiver<
        crate::models::clarification_store::ClarificationNotification,
    >,
    cancel_flag: Arc<AtomicBool>,
    progress_slot: InvokeAgentProgressSlot,
    policy: TurnPolicy,
}

impl PreparedTurn {
    /// Open the stream and drive it.
    async fn run<F: FnMut(SessionEvent)>(self, mut emit: F) {
        // Stages 1-3 of context shaping are free; stages 4-5 need an LLM
        // call, so no agent is passed and shaping caps at stage 3.
        let shaped = shape_context(self.history, &ContextShaperSettings::default(), None).await;
        if let Some(stage) = shaped.stage_applied {
            tracing::debug!(stage = ?stage, chars_freed = shaped.chars_freed, "context shaper applied before stream");
        }

        let stream = stream_prompt(
            &self.agent,
            shaped.messages,
            self.contents,
            Some(self.approval_rx),
            Some(self.resolution_rx),
            Some(self.clarification_rx),
            self.policy.max_agent_turns,
        )
        .await;
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                // The turn still starts and ends, so the owner finalizes it
                // like any other and the user message is rolled back under
                // the empty-turn rule.
                emit(SessionEvent::TurnStarted);
                emit(SessionEvent::Error(StreamError::new(
                    StreamErrorKind::Other,
                    format!("Failed to start stream: {e:#}"),
                )));
                emit(SessionEvent::TurnEnded);
                return;
            }
        };

        drive(
            stream,
            Vec::new(),
            self.task_controller,
            self.cancel_flag,
            self.progress_slot,
            self.policy,
            emit,
        )
        .await;
    }
}

/// Drive an open stream to its end through the session handler.
///
/// `queued_progress` is delivered ahead of the stream (tests only; in
/// production the tool sends progress while the provider is mid-response).
async fn drive<F: FnMut(SessionEvent)>(
    mut stream: crate::services::llm_service::ResponseStream,
    queued_progress: Vec<crate::tools::invoke_agent_tool::InvokeAgentProgress>,
    task_controller: AgentTaskController,
    cancel_flag: Arc<AtomicBool>,
    progress_slot: InvokeAgentProgressSlot,
    policy: TurnPolicy,
    emit: F,
) {
    let mut progress_rx = install_progress_channel(&progress_slot);
    if !queued_progress.is_empty() {
        let sender = progress_slot.lock().clone();
        if let Some(sender) = sender {
            for progress in queued_progress {
                let _ = sender.send(progress);
            }
        }
    }

    let mut handler = SessionStreamHandler::new(emit, task_controller, cancel_flag.clone(), policy);
    if let Err(e) = run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler).await
    {
        // The handler never fails a chunk, so the loop only errors on paths
        // that already emitted `Error` and `TurnEnded`.
        tracing::warn!(error = ?e, "stream loop returned an error after the turn ended");
    }

    // Clear the progress slot sender so stale references don't accumulate.
    *progress_slot.lock() = None;
}

// ---------------------------------------------------------------------------
// Scripted turns (tests and the frontends' adapter characterizations)
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
impl AgentSession {
    /// [`begin_turn`](Self::begin_turn) against a scripted stream instead of
    /// the provider: the same preparation, the same handler, the same
    /// events, with the network taken out.
    pub fn begin_scripted_turn<F: FnMut(SessionEvent)>(
        &mut self,
        input: TurnInput,
        scenario: crate::services::Scenario,
        emit: F,
    ) -> Result<impl Future<Output = ()> + use<F>> {
        let turn = self.prepare_turn(input, Arc::new(AtomicBool::new(false)))?;
        let stream = crate::services::scripted_stream(scenario.items, turn.cancel_flag.clone());
        Ok(drive(
            stream,
            scenario.progress,
            turn.task_controller,
            turn.cancel_flag,
            turn.progress_slot,
            turn.policy,
            emit,
        ))
    }
}

/// Run a scenario through the session handler alone — no conversation, no
/// agent — and return the events it produced. This is what the frontends'
/// adapter characterizations replay through `From<SessionEvent>`.
#[cfg(any(test, feature = "test-support"))]
pub async fn replay_scenario(
    scenario: crate::services::Scenario,
    policy: TurnPolicy,
) -> Vec<SessionEvent> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let stream = crate::services::scripted_stream(scenario.items, cancel_flag.clone());
    let progress_slot: InvokeAgentProgressSlot = Arc::new(parking_lot::Mutex::new(None));

    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    drive(
        stream,
        scenario.progress,
        AgentTaskController::new(),
        cancel_flag,
        progress_slot,
        policy,
        move |event| sink.borrow_mut().push(event),
    )
    .await;

    Rc::try_unwrap(events)
        .expect("the handler was dropped with the drive future")
        .into_inner()
}

#[cfg(test)]
mod tests;
