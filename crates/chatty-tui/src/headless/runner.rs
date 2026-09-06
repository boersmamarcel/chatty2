//! The headless runner: one conversation driven from an `AgentSession`
//! directly, with no terminal state behind it (AGE-196).
//!
//! `--headless` and `--pipe` used to build the interactive `ChatEngine` to
//! run a turn with no terminal attached. This is what they actually need:
//! the session (the turn, the approval stores, the cancel flag), the
//! transcript the answer-file heuristics read tool evidence from, and the
//! event channel `run_headless` drains. Pickers, scroll state and the chat
//! rectangle stay in the engine.
//!
//! Every `SessionEvent` the turn produces is also written to stderr as a
//! `CHATTY_EVENT` line (see `chatty_core::tools::format_event_line`), which
//! is how a parent `sub_agent` tool follows this process: the session's own
//! contract crossing the process boundary. Everything else on stderr is
//! the human-readable log.

use anyhow::{Context, Result};
use chatty_core::models::TurnOutcome;
use chatty_core::services::StreamSurface;
use chatty_core::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput, TurnKind};
use chatty_core::settings::models::ExecutionSettingsModel;
use chatty_core::tools::format_event_line;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

use crate::engine::{AgentContextInputs, ChatEngineConfig, Transcript, build_agent_context};
use crate::events::AppEvent;

/// Where the runner writes its `CHATTY_EVENT` lines. Stderr in production;
/// a test hands in a collector.
pub type EventLineWriter = Arc<dyn Fn(String) + Send + Sync>;

pub struct HeadlessRunner {
    pub session: AgentSession,
    /// The settings the next turn runs under. Headless recovery narrows
    /// `max_agent_turns` between turns, so this is the runner's own copy.
    pub execution_settings: ExecutionSettingsModel,
    pub transcript: Transcript,
    pub is_streaming: bool,
    pub is_ready: bool,
    config: ChatEngineConfig,
    skill_service: chatty_core::services::SkillService,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    event_line_writer: EventLineWriter,
    /// An agent-protocol follow-up that arrived while a turn was already
    /// streaming; sent once the turn ends (AGE-242 / D3).
    pending_agent_follow_up: Option<String>,
}

impl HeadlessRunner {
    pub fn new(config: ChatEngineConfig, event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        let skill_service =
            chatty_core::services::SkillService::new(config.embedding_service.clone());
        let session = AgentSession::new(AgentSessionConfig {
            execution_settings: config.execution_settings.clone(),
            surface: StreamSurface::Headless,
            // `run_headless` runs its own loop guard over the events, with
            // the answer-file deadline the session's does not know about.
            loop_guard: false,
        });
        Self {
            session,
            execution_settings: config.execution_settings.clone(),
            transcript: Transcript::new(),
            is_streaming: false,
            is_ready: false,
            config,
            skill_service,
            event_tx,
            event_line_writer: Arc::new(|line| eprintln!("{line}")),
            pending_agent_follow_up: None,
        }
    }

    /// Redirect the `CHATTY_EVENT` lines (tests).
    #[cfg(test)]
    pub fn set_event_line_writer(&mut self, writer: EventLineWriter) {
        self.event_line_writer = writer;
    }

    /// Build the agent (with the session's store handles) and its conversation.
    pub async fn init_conversation(&mut self) -> Result<()> {
        let mcp_tools = match self.config.mcp_service {
            Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
            None => None,
        };
        let mut ctx = build_agent_context(AgentContextInputs {
            execution_settings: &self.execution_settings,
            module_settings: &self.config.module_settings,
            models: &self.config.models,
            user_secrets: &self.config.user_secrets,
            memory_service: &self.config.memory_service,
            skill_service: &self.skill_service,
            search_settings: &self.config.search_settings,
            embedding_service: &self.config.embedding_service,
            remote_agents: &self.config.remote_agents,
            module_agents: &self.config.module_agents,
            is_sub_agent: self.config.is_sub_agent,
        });
        ctx.mcp_tools = mcp_tools;

        self.session
            .create_conversation(
                uuid::Uuid::new_v4().to_string(),
                "New Chat".to_string(),
                &self.config.model_config,
                &self.config.provider_config,
                ctx,
            )
            .await
            .context("Failed to create conversation")?;
        self.is_ready = true;
        Ok(())
    }

    /// Send a message and start streaming the response. A no-op while a
    /// turn is streaming or before the conversation exists.
    pub fn send_message(&mut self, message: String) {
        let Some(input) = self.prepare_send(message, true) else {
            return;
        };
        self.spawn_turn(input);
    }

    /// Inject a protocol / loop-guard follow-up without a user row.
    fn send_protocol_follow_up(&mut self, prompt: String) {
        let Some(input) = self.prepare_send(prompt, false) else {
            return;
        };
        self.spawn_turn(input);
    }

    pub(super) fn prepare_send(
        &mut self,
        message: String,
        show_in_transcript: bool,
    ) -> Option<TurnInput> {
        if !self.is_ready || self.is_streaming || self.session.conversation().is_none() {
            return None;
        }
        let kind = if chatty_core::services::is_protocol_follow_up_text(&message) {
            TurnKind::ProtocolFollowUp
        } else {
            TurnKind::Human
        };
        self.transcript.reset_sub_agent_row();
        if show_in_transcript {
            self.transcript.push_user(message.clone());
        }
        self.transcript.start_assistant();
        self.is_streaming = true;
        self.session.set_config(AgentSessionConfig {
            execution_settings: self.execution_settings.clone(),
            ..self.session.config().clone()
        });
        Some(TurnInput {
            kind,
            ..TurnInput::text(message)
        })
    }

    fn spawn_turn(&mut self, input: TurnInput) {
        match self.session.begin_turn(input, self.event_sink()) {
            Ok(turn) => {
                tokio::spawn(turn);
            }
            Err(e) => {
                warn!(error = ?e, "Failed to start the turn");
                self.is_streaming = false;
            }
        }
    }

    /// The sink a turn emits into: the event goes out to a parent process
    /// as a `CHATTY_EVENT` line, then to `run_headless` as an `AppEvent`.
    pub(crate) fn event_sink(&self) -> impl FnMut(SessionEvent) + Send + 'static {
        let event_tx = self.event_tx.clone();
        let writer = self.event_line_writer.clone();
        move |event| {
            if let Some(line) = format_event_line(&event) {
                writer(line);
            }
            let _ = event_tx.send(AppEvent::from(event));
        }
    }

    /// Stop the active turn. A blocked `ask_user` never reaches the loop's
    /// cancel check, so its pending request is dropped too.
    pub fn stop_stream(&mut self) {
        self.session.cancel();
        self.session.clarifications().cancel_all();
    }

    /// Fold a stream event into the session and the transcript.
    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::StreamStarted => self.is_streaming = true,
            AppEvent::TextChunk(text) => {
                self.session.append_streaming_text(&text);
                self.transcript.push_text(&text);
            }
            AppEvent::ToolCallStarted { id, name } => {
                self.session.note_tool_started(&id, &name);
                self.transcript.tool_started(id, name);
            }
            AppEvent::ToolCallInput { id, arguments } => {
                self.session.note_tool_input(&id, &arguments);
                self.transcript.tool_input(&id, &arguments);
            }
            AppEvent::ToolCallResult { id, result } => {
                self.session.note_tool_result(&id, &result);
                self.transcript.tool_result(&id, result);
            }
            AppEvent::ToolCallError { id, error } => {
                self.session.note_tool_error(&id, &error);
                self.transcript.tool_error(&id, error);
            }
            AppEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => self
                .session
                .note_approval_requested(&id, &command, is_sandboxed),
            AppEvent::ApprovalResolved { id, approved } => {
                self.session.note_approval_resolved(&id, approved)
            }
            // Nobody can answer in headless mode: unblock the tool now rather
            // than letting it wait out its timeout.
            AppEvent::ClarificationRequested { id, questions } => {
                self.session.note_clarification_requested(&id, &questions);
                eprintln!(
                    "The agent asked a clarifying question; headless mode has no one to answer."
                );
                self.session.clarifications().cancel_all();
            }
            AppEvent::TokenUsage(usage) => self.session.record_turn_usage(usage),
            AppEvent::TurnMessages(messages) => self.session.set_turn_messages(messages),
            AppEvent::SubAgent(progress) => {
                self.session.note_sub_agent(&progress);
                let line = crate::engine::helpers::sub_agent_line(&progress);
                if matches!(
                    progress,
                    chatty_core::tools::invoke_agent_tool::InvokeAgentProgress::Finished { .. }
                ) {
                    self.transcript.sub_agent_finished(line);
                } else {
                    let line = crate::engine::sanitize_progress_line(&line);
                    if !line.is_empty() {
                        self.transcript.sub_agent_progress(line);
                    }
                }
            }
            AppEvent::StreamCompleted => {
                self.transcript.finish_streaming();
                self.finish_turn();
                self.send_pending_agent_follow_up();
            }
            AppEvent::StreamError(error) => {
                self.transcript.mark_error(&error.to_string());
                self.finish_turn();
            }
            AppEvent::StreamCancelled => {
                self.transcript.mark_cancelled();
                self.finish_turn();
                self.send_pending_agent_follow_up();
            }
            AppEvent::AgentProtocolFollowUp(prompt) => {
                self.transcript
                    .add_system(format!("Agent protocol follow-up: {prompt}"));
                if !self.is_streaming {
                    self.send_protocol_follow_up(prompt);
                } else if self.pending_agent_follow_up.is_none() {
                    self.pending_agent_follow_up = Some(prompt);
                } else {
                    warn!(
                        "Dropping a later agent protocol follow-up; an earlier one is already queued"
                    );
                }
            }
            // Lifecycle and terminal events are the interactive app's.
            _ => {}
        }
    }

    /// Commit the turn (no trace, no artifacts in headless mode) and reset.
    /// A second call for the same turn is a no-op inside the session.
    fn finish_turn(&mut self) {
        if let Some(TurnOutcome::DroppedAndRolledBack(_)) = self.session.finish_turn(None, vec![]) {
            // Headless has no composer to restore into; the text is in the
            // transcript already.
        }
        self.is_streaming = false;
        self.session.clarifications().cancel_all();
    }

    fn send_pending_agent_follow_up(&mut self) {
        if let Some(prompt) = self.pending_agent_follow_up.take() {
            self.send_protocol_follow_up(prompt);
        }
    }
}
