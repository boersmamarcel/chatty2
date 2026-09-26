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
//! A parent that delegated this turn follows it through an
//! [`EventObserver`] — the broker participant's socket (AGE-301). Stderr is
//! the human-readable log and nothing else.

use anyhow::{Context, Result};
use chatty_core::factories::agent_factory::{
    AgentBuildContext, AgentServices, gated_exec_settings,
};
use chatty_core::models::TurnOutcome;
use chatty_core::models::clarification_store::ClarificationAnswer;
use chatty_core::services::StreamSurface;
use chatty_core::services::team::Team;
use chatty_core::services::turn_budget::{Deadline, TurnBudget};
use chatty_core::session::{
    AgentSession, AgentSessionConfig, Arrival, Decision, Mailbox, SessionEvent, TurnEnd, TurnInput,
    TurnKind,
};
use chatty_core::settings::models::ExecutionSettingsModel;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

use crate::engine::{ChatEngineConfig, Transcript};
use crate::events::AppEvent;

/// Takes the turn's events as they happen.
///
/// A child running as a broker participant (AGE-301) reports over its
/// socket, where the events are frames rather than text. Without an observer
/// the turn is silent to anyone outside this process — which is what plain
/// `--headless` is: an answer on stdout and a log on stderr.
pub type EventObserver = Arc<dyn Fn(&SessionEvent) + Send + Sync>;

/// Tool turns a follow-up pass gets once the run's `max_agent_turns` is
/// spent, and all a finalization pass ever gets: enough to write the answer
/// file, not to research again. A run spends at most `max_agent_turns` plus
/// this many tool turns across all its passes.
///
/// Six, not four: the finalization prompt allows one quick check before
/// `final_answer`, and that check is usually a script. One failed run of it
/// and its fix, a shell command that writes the answer file (which earns a
/// grace turn to read its output), then `final_answer` is already four, and
/// TurnBudget disables tools after the last. Six leaves one call of slack
/// and still bounds a `--max-agent-turns 50` run at 56.
pub(super) const FINAL_PASS_TOOL_TURNS: usize = 6;

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
    event_observer: Option<EventObserver>,
    /// Follow-ups that arrived while a turn was already streaming; sent
    /// once the turn ends (AGE-242 / D3, the mailbox of AGE-482).
    mailbox: Mailbox<String>,
    /// "read_skill <skill> and follow it", prepended to the first human turn
    /// of a `--team` run and then gone (AGE-407).
    pending_first_turn: Option<String>,
    /// The message of the last turn when it ended with nothing to keep and
    /// was rolled back off the history (AGE-243), for a retry to re-send.
    rolled_back_message: Option<String>,
    /// Tool turns (model calls that made tool calls) every pass of this run
    /// has spent so far. `max_agent_turns` is the run's budget, not each
    /// pass's: every follow-up headless sends is a new `stream_prompt`, and
    /// with a fresh budget each one run made 101 tool calls in 76 minutes
    /// under `--max-agent-turns 50`.
    pub(super) tool_turns_spent: usize,
    /// Whether the current model call already counted as a tool turn: a
    /// parallel batch is one turn, the next call starts after its results.
    in_tool_turn: bool,
    /// The next pass is a finalization and gets [`FINAL_PASS_TOOL_TURNS`].
    pub(super) next_pass_is_final: bool,
    /// The run's wall-clock budget (`--max-duration`), started by
    /// `run_headless` when the run begins.
    pub(super) max_duration: Option<std::time::Duration>,
    /// The running clock of that budget, once started.
    pub(super) deadline: Option<Deadline>,
    /// Whether the task asks for an answer file, once `note_task` has seen
    /// it; the agent is built with it (`AgentBuildContext::answer_file`).
    pub(super) answer_file: Option<bool>,
    /// Tests only: the budget every turn started with, in order.
    #[cfg(test)]
    pub(super) scripted_budgets: Vec<Option<TurnBudget>>,
    /// Tests only: each turn plays the next of these instead of calling the
    /// provider, so `run_headless` can be driven end to end offline.
    #[cfg(test)]
    pub(super) scripted_turns: std::collections::VecDeque<chatty_core::services::Scenario>,
    /// Tests only: the text of every turn started, in order.
    #[cfg(test)]
    pub(super) scripted_inputs: Arc<std::sync::Mutex<Vec<String>>>,
    /// Tests only: retry a failed turn at once instead of after the
    /// policy's delay.
    #[cfg(test)]
    pub(super) skip_recovery_delay: bool,
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
        let pending_first_turn = config.team.as_ref().and_then(Team::first_turn_instruction);
        Self {
            session,
            execution_settings: config.execution_settings.clone(),
            transcript: Transcript::new(),
            is_streaming: false,
            is_ready: false,
            config,
            skill_service,
            event_tx,
            event_observer: None,
            mailbox: Mailbox::new(),
            pending_first_turn,
            rolled_back_message: None,
            tool_turns_spent: 0,
            in_tool_turn: false,
            next_pass_is_final: false,
            max_duration: None,
            deadline: None,
            answer_file: None,
            #[cfg(test)]
            scripted_budgets: Vec::new(),
            #[cfg(test)]
            scripted_turns: Default::default(),
            #[cfg(test)]
            scripted_inputs: Default::default(),
            #[cfg(test)]
            skip_recovery_delay: false,
        }
    }

    /// Give the run a wall-clock budget (`--max-duration`); the clock
    /// starts with the run, not here.
    pub fn set_max_duration(&mut self, budget: Option<std::time::Duration>) {
        self.max_duration = budget;
    }

    /// Tell the runner its task before the agent is built (`--headless
    /// --message`), so `final_answer` only writes an answer file when the
    /// task asks for one — the same test `run_headless` applies.
    pub fn note_task(&mut self, message: &str) {
        self.answer_file = Some(super::answer_file::prompt_requires_answer_file(&[
            message,
            self.role_preamble().unwrap_or_default(),
        ]));
    }

    /// Start the run's clock, if it has a budget.
    pub(super) fn start_clock(&mut self) -> Option<Deadline> {
        self.deadline = self.max_duration.map(Deadline::starting_now);
        self.deadline
    }

    /// Send the run's last pass after its time ran out mid-turn: one
    /// tool-free call on the same history, so the model answers with what
    /// it has. Its tool turns count as spent, so the pass has none.
    pub(super) fn send_time_up_pass(&mut self, prompt: String) {
        let Some(input) = self.prepare_send(prompt, false) else {
            return;
        };
        let total = self.execution_settings.max_agent_turns as usize;
        self.spawn_turn(TurnInput {
            kind: TurnKind::ProtocolFollowUp,
            turn_budget: Some(TurnBudget::run_share(0, total, self.tool_turns_spent)),
            ..input
        });
    }

    /// Send the turn's events to `observer` as they happen (AGE-301).
    pub fn set_event_observer(&mut self, observer: EventObserver) {
        self.event_observer = Some(observer);
    }

    /// Whether this process leads a `--team` run (AGE-441). A worker never
    /// gets `--team`, so it — like a lone `--headless` agent — is not one.
    pub fn is_team_leader(&self) -> bool {
        self.config.team.is_some()
    }

    /// This role's preamble (`--preamble`, or a team leader's/worker's own
    /// declared one), if any. An instruction like "write your answer to
    /// /app/answer.txt" can arrive here instead of in `--message`, which the
    /// answer-file heuristics need to check too (AGE evidence: FinanceAgent
    /// trials that only got the instruction via `--preamble`).
    pub(super) fn role_preamble(&self) -> Option<&str> {
        self.config.role.preamble.as_deref()
    }

    /// Build the agent (with the session's store handles) and its conversation.
    pub async fn init_conversation(&mut self) -> Result<()> {
        let mcp_tools = match self.config.mcp_service {
            Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
            None => None,
        };
        let ctx = AgentBuildContext {
            mcp_tools,
            role: self.config.role.clone(),
            team_skill: self.config.team.as_ref().and_then(Team::skill),
            unattended: true,
            answer_file: self.answer_file,
            // Ungated: see `ChatEngine::build_agent_context`.
            ask_user_enabled: self.execution_settings.ask_user_enabled,
            ..AgentBuildContext::from_services(AgentServices {
                exec_settings: gated_exec_settings(&self.execution_settings),
                user_secrets: self.config.user_secrets.clone(),
                memory_service: self.config.memory_service.clone(),
                skill_service: Some(self.skill_service.clone()),
                search_settings: self.config.search_settings.clone(),
                embedding_service: self.config.embedding_service.clone(),
                module_agents: self.config.module_agents.clone(),
                gateway_port: self.config.broker_port.or(self
                    .config
                    .module_settings
                    .enabled
                    .then_some(self.config.module_settings.gateway_port)),
                local_agents: match self.config.team.as_ref() {
                    Some(team) => team.agent_names(),
                    None => self.config.module_settings.virtual_agent_names(),
                },
                remote_agents: self.config.remote_agents.clone(),
            })
        };

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

    /// The message of the last turn if it ended empty and was rolled back
    /// off the history: a stream error before the model said anything
    /// takes the prompt with it, so a retry must send it again rather than
    /// ask the model to continue.
    pub fn take_rolled_back_message(&mut self) -> Option<String> {
        self.rolled_back_message
            .take()
            .filter(|message| !message.trim().is_empty())
    }

    /// Re-prompt after a stream error. Shown like a user turn but not a
    /// human one: the session's recovery budget resets only on those
    /// (AGE-273).
    pub fn send_recovery_prompt(&mut self, prompt: String) {
        let Some(input) = self.prepare_send(prompt, true) else {
            return;
        };
        self.spawn_turn(TurnInput {
            kind: TurnKind::ProtocolFollowUp,
            ..input
        });
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
        // A `--team` leader's first human turn opens with the skill to
        // follow (AGE-407). Taken only by a human turn, so a protocol
        // follow-up arriving first leaves it for the human turn after.
        let message = match kind {
            TurnKind::Human => match self.pending_first_turn.take() {
                Some(instruction) => format!("{instruction}\n\n{message}"),
                None => message,
            },
            _ => message,
        };
        self.transcript.reset_delegation_row();
        if show_in_transcript {
            self.transcript.push_user(message.clone());
        }
        self.transcript.start_assistant();
        self.is_streaming = true;
        self.session.set_config(AgentSessionConfig {
            execution_settings: self.execution_settings.clone(),
            ..self.session.config().clone()
        });
        let final_pass = std::mem::take(&mut self.next_pass_is_final);
        Some(TurnInput {
            kind,
            turn_budget: Some(self.pass_turn_budget(final_pass)),
            ..TurnInput::text(message)
        })
    }

    /// The budget of the next pass: what is left of the run's
    /// `max_agent_turns`, at least [`FINAL_PASS_TOOL_TURNS`] once the run has
    /// spent any (so a follow-up can still write the answer), never more
    /// than `max_agent_turns + FINAL_PASS_TOOL_TURNS` over the whole run,
    /// and at most [`FINAL_PASS_TOOL_TURNS`] (at least one, for
    /// `final_answer`, even past that ceiling) for a finalization pass. An
    /// uncapped (`0`) run's passes are uncapped too, but a finalization
    /// still gets only [`FINAL_PASS_TOOL_TURNS`]. Every pass carries the
    /// run's clock until it runs out; the passes after that are the run's
    /// last and bounded by their own budgets.
    pub(super) fn pass_turn_budget(&self, final_pass: bool) -> TurnBudget {
        let deadline = self
            .deadline
            .filter(|d| !d.is_past(std::time::Instant::now()));
        let total = self.execution_settings.max_agent_turns as usize;
        if total == 0 {
            let budget = if final_pass {
                TurnBudget::run_share(FINAL_PASS_TOOL_TURNS, 0, self.tool_turns_spent)
            } else {
                TurnBudget::new(0)
            };
            return budget.with_deadline(deadline);
        }
        let spent = self.tool_turns_spent;
        let mut turns = if spent == 0 {
            total
        } else {
            total
                .saturating_sub(spent)
                .max(FINAL_PASS_TOOL_TURNS)
                .min((total + FINAL_PASS_TOOL_TURNS).saturating_sub(spent))
        };
        if final_pass {
            // At least one: a finalization pass exists to call
            // `final_answer`, and with no tool turn it can only answer in
            // text, which writes no answer file. `MAX_FINALIZATION_ATTEMPTS`
            // bounds what this adds past the run's ceiling.
            turns = turns.clamp(1, FINAL_PASS_TOOL_TURNS);
        }
        TurnBudget::run_share(turns, total, spent).with_deadline(deadline)
    }

    /// Whether a follow-up pass would still get a tool turn: an uncapped
    /// run always does, a capped one until it has spent `max_agent_turns`
    /// plus [`FINAL_PASS_TOOL_TURNS`].
    pub(super) fn has_tool_turns_left(&self) -> bool {
        let total = self.execution_settings.max_agent_turns as usize;
        total == 0 || self.tool_turns_spent < total + FINAL_PASS_TOOL_TURNS
    }

    fn spawn_turn(&mut self, input: TurnInput) {
        self.in_tool_turn = false;
        #[cfg(test)]
        self.scripted_budgets.push(input.turn_budget);
        #[cfg(test)]
        if let Some(scenario) = self.scripted_turns.pop_front() {
            let text = input
                .contents
                .iter()
                .filter_map(|content| match content {
                    rig_core::completion::message::UserContent::Text(text) => {
                        Some(text.text.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.scripted_inputs.lock().unwrap().push(text);
            let turn = self
                .session
                .begin_scripted_turn(input, scenario, self.event_sink())
                .expect("scripted turn starts");
            tokio::spawn(turn);
            return;
        }
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

    /// The sink a turn emits into: the event goes to the observer, if a
    /// parent installed one, and then to `run_headless` as an `AppEvent`.
    pub(crate) fn event_sink(&self) -> impl FnMut(SessionEvent) + Send + 'static {
        let event_tx = self.event_tx.clone();
        let observer = self.event_observer.clone();
        move |event| {
            if let Some(observer) = observer.as_ref() {
                observer(&event);
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
                self.in_tool_turn = false;
                self.session.append_streaming_text(&text);
                self.transcript.push_text(&text);
            }
            AppEvent::ToolCallStarted { id, name } => {
                if !self.in_tool_turn {
                    self.in_tool_turn = true;
                    self.tool_turns_spent += 1;
                }
                self.session.note_tool_started(&id, &name);
                self.transcript.tool_started(id, name);
            }
            AppEvent::ToolCallInput { id, arguments } => {
                self.session.note_tool_input(&id, &arguments);
                self.transcript.tool_input(&id, &arguments);
            }
            AppEvent::ToolCallResult { id, result } => {
                self.in_tool_turn = false;
                self.session.note_tool_result(&id, &result);
                self.transcript.tool_result(&id, result);
            }
            AppEvent::ToolCallError { id, error } => {
                self.in_tool_turn = false;
                self.session.note_tool_error(&id, &error);
                self.transcript.tool_error(&id, error);
            }
            AppEvent::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => {
                self.session
                    .note_approval_requested(&id, &command, is_sandboxed);
                self.transcript
                    .approval_requested(id, command, is_sandboxed);
            }
            AppEvent::ApprovalResolved { id, approved } => {
                self.session.note_approval_resolved(&id, approved);
                self.transcript.approval_resolved(&id, approved);
            }
            AppEvent::ClarificationRequested { id, questions } => {
                self.session.note_clarification_requested(&id, &questions);
                if self.event_observer.is_some() {
                    // A parent is following this turn: the question has gone
                    // up the chain as `input-required`, and the answer comes
                    // back through the participant loop (AGE-306).
                    eprintln!("The agent asked a clarifying question; waiting for the parent.");
                } else if self.is_team_leader() {
                    // A `--team` leader is the top of its own chain: nothing
                    // is attended to relay this to (its own event_observer is
                    // always None — see `is_team_leader`'s doc), whether the
                    // question is the leader's own or one AGE-306 relayed up
                    // from a delegated worker's `ask_user`. `cancel_all()`
                    // would surface as a hard failure of whichever turn asked
                    // (AGE-452), so answer with a default instead of failing.
                    eprintln!(
                        "The agent asked a clarifying question; no human is available in this \
                         headless --team run, answering with a default."
                    );
                    let answers = questions
                        .iter()
                        .map(|q| ClarificationAnswer {
                            id: q.id.clone(),
                            answer: "No human is available to answer this; use your best \
                                     judgment and proceed."
                                .to_string(),
                            custom: true,
                        })
                        .collect();
                    self.session.clarifications().resolve(&id, answers);
                } else {
                    // Nobody can answer in plain headless mode: unblock the
                    // tool now rather than letting it wait out its timeout.
                    eprintln!(
                        "The agent asked a clarifying question; headless mode has no one to answer."
                    );
                    self.session.clarifications().cancel_all();
                }
            }
            AppEvent::TokenUsage(usage) => self.session.record_turn_usage(usage),
            AppEvent::TurnMessages(messages) => self.session.set_turn_messages(messages),
            AppEvent::Delegation(progress) => {
                self.session.note_delegation(&progress);
                let line = crate::engine::helpers::delegation_line(&progress);
                if matches!(
                    progress,
                    chatty_core::tools::invoke_agent_tool::InvokeAgentProgress::Finished { .. }
                ) {
                    self.transcript.delegation_finished(line);
                } else {
                    let line = crate::engine::sanitize_progress_line(&line);
                    if !line.is_empty() {
                        self.transcript.delegation_progress(line);
                    }
                }
            }
            AppEvent::StreamCompleted => {
                self.transcript.finish_streaming();
                self.finish_turn();
                self.drain_mailbox(TurnEnd::Completed);
            }
            AppEvent::StreamError(error) => {
                self.transcript.mark_error(&error.to_string());
                self.finish_turn();
                self.drain_mailbox(TurnEnd::Error);
            }
            AppEvent::StreamCancelled => {
                self.transcript.mark_cancelled();
                self.finish_turn();
                self.drain_mailbox(TurnEnd::Cancelled);
            }
            AppEvent::AgentProtocolFollowUp(prompt) => {
                self.transcript
                    .add_system(format!("Agent protocol follow-up: {prompt}"));
                match self
                    .mailbox
                    .arrive(Arrival::FollowUp(prompt), self.is_streaming)
                {
                    Decision::Dispatch(next) => self.send_protocol_follow_up(next.message),
                    Decision::Refused(refusal) => {
                        warn!(%refusal, "Dropping a later agent protocol follow-up");
                    }
                    _ => {}
                }
            }
            // Lifecycle and terminal events are the interactive app's.
            _ => {}
        }
    }

    /// Commit the turn (no trace, no artifacts in headless mode) and reset.
    /// A second call for the same turn is a no-op inside the session.
    fn finish_turn(&mut self) {
        // Headless has no composer to restore a rolled-back message into;
        // it is kept for `take_rolled_back_message` instead.
        match self.session.finish_turn(None, vec![]) {
            Some(TurnOutcome::DroppedAndRolledBack(message)) => {
                self.rolled_back_message = Some(message);
            }
            Some(TurnOutcome::Persisted) => self.rolled_back_message = None,
            None => {}
        }
        self.is_streaming = false;
        self.session.clarifications().cancel_all();
    }

    /// The turn ended; send whatever the mailbox says is next.
    fn drain_mailbox(&mut self, end: TurnEnd) {
        if let Some(next) = self.mailbox.turn_ended(end) {
            self.send_protocol_follow_up(next.message);
        }
    }
}
