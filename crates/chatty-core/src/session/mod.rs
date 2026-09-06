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
use crate::models::clarification_store::{ClarificationStore, PendingClarifications};
use crate::models::conversation::{Conversation, TurnOutcome};
use crate::models::execution_approval_store::{ExecutionApprovalStore, PendingApprovals};
use crate::models::token_usage::TokenUsage;
use crate::models::write_approval_store::{PendingWriteApprovals, WriteApprovalStore};
use crate::services::{
    AgentTaskController, ContextShaperSettings, StreamError, StreamErrorKind, StreamSurface,
    exchange_count, extract_user_text, install_progress_channel, run_stream_loop, shape_context,
    stream_prompt,
};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::tools::invoke_agent_tool::InvokeAgentProgressSlot;

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
    /// Re-running the last user message, which is already the tail of the
    /// conversation's history: nothing is added to it.
    Regenerate,
}

/// What to send.
pub struct TurnInput {
    /// The user message: persisted (unless [`TurnKind::Regenerate`]) and
    /// sent. For `Regenerate` this is the history's last user message,
    /// which the caller has split off — `stream_prompt` appends it after
    /// the history it is given with no de-duplication (AGE-221).
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
        let turn = self.prepare_turn(input)?;
        Ok(turn.run(emit))
    }

    /// Everything `begin_turn` does before the stream is opened, kept apart
    /// so a test can run the same preparation against a scripted stream.
    fn prepare_turn(&mut self, input: TurnInput) -> Result<PreparedTurn> {
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
        // (AGE-221).
        let history = conversation.messages();
        let already_asked_to_retry = history
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User { content } => Some(extract_user_text(content)),
                _ => None,
            })
            .is_some_and(|text| text.trim_start().starts_with(MALFORMED_TOOL_CALL_FOLLOW_UP));

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
        // nudge forever and block a second write_todos (AGE-150).
        if kind == TurnKind::Human {
            task_controller.reset();
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

        let cancel_flag = Arc::new(AtomicBool::new(false));
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
    /// text, the turn's messages, the todo snapshot after a todo tool, and
    /// usage. Display state is the caller's, after this.
    pub fn apply(&mut self, event: &SessionEvent) {
        let Some(conversation) = self.conversation.as_mut() else {
            return;
        };
        match event {
            SessionEvent::Text(text) => conversation.append_streaming_content(text),
            SessionEvent::ToolCallStarted { id, name } => {
                self.pending_tool_names.insert(id.clone(), name.clone());
            }
            SessionEvent::ToolCallResult { id, .. } | SessionEvent::ToolCallError { id, .. } => {
                if let Some(name) = self.pending_tool_names.remove(id)
                    && is_agent_todo_tool(&name)
                {
                    let snapshot = conversation.agent().task_controller().snapshot();
                    conversation.set_agent_task_snapshot(Some(snapshot));
                }
            }
            SessionEvent::TurnMessages(messages) => {
                conversation.set_streaming_turn_messages(Some(messages.clone()));
            }
            SessionEvent::TokenUsage(usage) => {
                self.last_turn_usage = Some(usage.clone());
            }
            _ => {}
        }
    }

    /// Commit the turn under the shared empty-turn rule (AGE-243 / D4) and
    /// clear per-turn state. `trace` and `artifacts` are the frontend's: the
    /// tool-call trace it rendered and the files `add_attachment` queued.
    ///
    /// Returns `None` when there is no conversation. A `DroppedAndRolledBack`
    /// outcome carries the text of the user message that was rolled back, for
    /// the caller to put back into its composer.
    pub fn finish_turn(
        &mut self,
        trace: Option<serde_json::Value>,
        artifacts: Vec<PathBuf>,
    ) -> Option<TurnOutcome> {
        self.cancel_flag = None;
        self.pending_tool_names.clear();
        // Unblock any `ask_user` still waiting, so a cancelled turn cannot
        // leave a tool parked until its timeout.
        self.clarifications.cancel_all();

        let conversation = self.conversation.as_mut()?;
        let response = conversation
            .streaming_message()
            .cloned()
            .unwrap_or_default();
        let outcome = conversation.finalize_turn(response, artifacts, trace);
        conversation.set_streaming_message(None);

        if let Some(usage) = self.last_turn_usage.clone() {
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
        let turn = self.prepare_turn(input)?;
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
