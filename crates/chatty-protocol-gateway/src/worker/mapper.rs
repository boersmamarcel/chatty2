//! `SessionEvent` → A2A, the interface AGE-301 exists to shape.
//!
//! [`SessionEvent`] is the output contract of a turn: one event per
//! observable thing the turn did. A2A is a *task* protocol: status updates
//! and artifact updates. ADR-0011's first kill criterion is whether the
//! second can carry the first at the granularity the parent already renders,
//! and this table is the answer being tested.
//!
//! There is one table because there is one question. A worker is a child
//! process on the desktop and a microVM when hosted (AGE-307), and a parent
//! must not be able to tell which it delegated to from the transcript; two
//! copies of this file would make that a coincidence rather than a property.
//!
//! | `SessionEvent` | frame |
//! |---|---|
//! | `TurnStarted` | `status: working` |
//! | `Text` | `artifact` (a chunk, `lastChunk: false`) |
//! | `ToolCallStarted` / `ToolCallResult` / `ToolCallError` | `status: working` with the progress line |
//! | `ToolCallInput` | — |
//! | `ApprovalRequested` | a `human.approve` request, to the root (see below) |
//! | `ClarificationRequested` | a `human.ask` request, up the caller chain (see below) |
//! | `ApprovalResolved` | `status: working`, and a `req.cancel` of a request still open |
//! | `Delegation` | `status: working` (a grandchild's progress) |
//! | `ApiCallUsage` | — (folded into `TokenUsage`) |
//! | `TokenUsage` | — (held, and attached to the terminal status) |
//! | `TurnMessages` | — (held when capture is on, and attached to the terminal status; RC-0, AGE-649) |
//! | `Error` / `Cancelled` | `status: working`, and the task's recorded outcome |
//! | `TurnEnded` | — (see below) |
//! | `FollowUp` | — |
//!
//! # Three decisions worth naming
//!
//! **Text becomes an artifact, not a status message.** A2A artifacts are the
//! task's output; status messages are progress about it. Putting the
//! assistant's answer in status messages would make it unrecoverable to a
//! caller that only reads artifacts, which is what `A2aClient` does to build
//! a response.
//!
//! **The progress lines come from `chatty_core`'s
//! [`progress_text_for_event`].** The parent renders a worker's tool calls
//! from the worker's own events; rewriting the strings here would put a
//! second, drifting copy of them in the transcript.
//!
//! **`TurnEnded` produces nothing.** A terminal status ends the A2A task, and
//! one delegated task can span several turns — headless recovery and
//! protocol follow-ups each end a turn. The participant sends exactly one
//! terminal status when the whole delegation is over, built from
//! [`TaskMapper::terminal`].
//!
//! # A question goes up the chain
//!
//! `ClarificationRequested` is the worker's `ask_user` waiting on someone.
//! It becomes a `human.ask` request carrying every question with its
//! options, numbered by this mapper (ADR-0021 § 2, EN-2b). It names no
//! asker — the broker stamps that from the connection — and no origin: an
//! `ask_user` call's arguments cannot say the question came from anyone
//! else. The broker relays it up the caller chain, and its answers come
//! back as the request's result, which [`answer_inputs`] hands to the
//! clarification store under the id the question was raised with; the tool
//! result that follows is what un-parks the task.
//!
//! # An approval goes to the root
//!
//! An `ApprovalRequested` does not go up the chain: only the root answers
//! approvals (ADR-0021 § 2, EN-2a). It becomes a `human.approve` request
//! with the kind, the command or path and a write's diff stat, numbered by
//! this mapper; the broker delivers it to the root and the root's verdict
//! comes back as the request's result. [`answer_inputs`] resolves it on the
//! store it came from — the execution or the write store — under the id it
//! was raised with, and nowhere else: an answer to a question never
//! resolves an approval. An approval the worker stops waiting on (its own
//! timeout, a cancelled turn) is withdrawn with `req.cancel`.
//!
//! # What A2A cannot carry
//!
//! Token usage. A2A has no notion of it, and inventing a frame would put
//! accounting into the task protocol. It rides in the terminal status's
//! `metadata`, which is where ADR-0011's ledger (AGE-307) reads it. It goes
//! as lines, one per model, each naming its model and carrying tokens and
//! time but never a price (AGE-682): whoever reads a line prices it. What
//! this worker's own delegations spent is forwarded with it (AGE-415), each
//! line still naming the agent below this one that spent it (TB-3), and
//! lines merge only when they share both agent and model. So the root sees
//! one line per agent and model however deep the tree below it, and its
//! lines carry the whole tree. Nothing is both forwarded and folded.
//!
//! # A nested run's events (TB-1, AGE-663)
//!
//! A task sent with `swarmEvents` — a run nested under a listening root —
//! also reports its own turns and tool events as `event` frames
//! ([`TaskMapper::swarm_event`]), which the broker tags and forwards to the
//! root. They are the worker's own: a grandchild's steps this worker
//! passes up as `Delegation` progress are not among them, since the
//! grandchild reports its own. Off by default, and then no `event` frame
//! is ever built.
//!
//! # Conversation capture (RC-0, AGE-649)
//!
//! A resumable worker needs its own history back, not just its answer. This
//! is opt-in per task — the broker sets `DelegatedTask::capture_conversation`
//! when it hands a worker its task — and off by default, so an ordinary
//! delegation's frames are byte-identical to what they were before this
//! existed. When it is on, every `TurnMessages` event's messages accumulate
//! in arrival order (the same order `Conversation::finalize_turn` persists
//! them in, AGE-247) and ride the terminal status under
//! [`CONVERSATION_METADATA_KEY`], capped at 32 MB
//! (`fabric-resumable-conversations` §4.1). Over the cap, the byte count
//! rides under [`CONVERSATION_TOO_LARGE_METADATA_KEY`] instead — never a
//! silently truncated conversation.
//!
//! # Typed handoffs (TD-2, AGE-693)
//!
//! A task whose role has a handoff schema carries it
//! ([`with_handoff`](TaskMapper::with_handoff)). The mapper keeps the text of
//! the turn's last model call and, at the terminal status of a task that
//! completed, checks it against the schema: a valid handoff rides the
//! status under [`HANDOFF_METADATA_KEY`]; an invalid one fails the task,
//! the role and errors under [`HANDOFF_INVALID_METADATA_KEY`]. The one
//! re-prompt the worker gets in between is its runner's (it reaches the
//! mapper as a `FollowUp`, which is how the invalid answers are counted
//! under [`HANDOFF_INVALID_COUNT_METADATA_KEY`]). A task without a schema
//! keeps none of this, so its frames are the bytes they always were.

use crate::participant::{ParticipantFrame, TaskState};
use chatty_core::models::clarification_store::{ClarificationAnswer, ClarificationStore};
use chatty_core::models::execution_approval_store::{
    self as approvals, ApprovalDecision, ApprovalDetail, ExecutionApprovalStore,
};
use chatty_core::models::token_usage::TokenUsage;
use chatty_core::models::write_approval_store::{WriteApprovalDecision, WriteApprovalStore};
use chatty_core::services::a2a_client::wire_usage;
use chatty_core::services::handoff::{self, HandoffContract, HandoffOutcome};
use chatty_core::session::SessionEvent;
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_core::tools::progress_text_for_event;
use chatty_fabric::wire::{HandoffInvalid, Opaque, TaskMetadata, WorkerSwarmItem};
use chatty_fabric::{Answer, ApprovalKind, ApprovalRequest, ApprovalVerdict, AskRequest, Question};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::warn;

/// Answers for the running task, as the broker delivers them.
pub type InputReceiver = mpsc::UnboundedReceiver<WorkerInput>;

/// One answer for the running task.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerInput {
    /// The answers to one of the task's `ask_user` questions: the result
    /// of its `human.ask` (EN-2b). `id` is the question's id on the
    /// clarification store.
    Answer { id: String, answers: Vec<Answer> },
    /// Nobody up the caller chain answered that question: its `ask_user`
    /// fails.
    Unanswered { id: String },
    /// The root's verdict on one of the task's approvals: the result of its
    /// `human.approve` (EN-2a). `id` is the approval's id on the store
    /// `kind` names.
    Approval {
        id: String,
        kind: ApprovalKind,
        verdict: ApprovalVerdict,
    },
}

/// The stores a worker's turn waits on for its caller's answers: its
/// `ask_user` questions and its execution and write approvals (AGE-306,
/// AGE-646). Clones of the session's own — they share its state.
#[derive(Clone)]
pub struct InputStores {
    pub clarifications: ClarificationStore,
    pub execution_approvals: ExecutionApprovalStore,
    pub write_approvals: WriteApprovalStore,
}

/// Hand every answer the broker sends down to the store the worker's turn
/// is waiting on, until the task is over: a question's answers to the
/// clarification store, an approval's verdict to the store it was raised
/// on. Each resolves only its own store (EN-2a): an answer to a question
/// that reads `approve` grants nothing.
///
/// The embedder spawns this beside its turn: the answers arrive on the
/// socket's read half while the turn runs, and the stores are the one thing
/// both the tools and this loop can reach.
pub async fn answer_inputs(mut inputs: InputReceiver, stores: InputStores) {
    while let Some(input) = inputs.recv().await {
        let (id, answered) = match input {
            WorkerInput::Answer { id, answers } => {
                let answered = stores
                    .clarifications
                    .resolve(&id, clarification_answers(answers));
                (id, answered)
            }
            WorkerInput::Unanswered { id } => {
                let answered = stores.clarifications.cancel(&id);
                (id, answered)
            }
            WorkerInput::Approval { id, kind, verdict } => {
                let answered = match kind {
                    ApprovalKind::Exec => stores.execution_approvals.resolve(
                        &id,
                        match verdict {
                            ApprovalVerdict::Approved => ApprovalDecision::Approved,
                            ApprovalVerdict::Denied => ApprovalDecision::Denied,
                        },
                    ),
                    ApprovalKind::Write => stores.write_approvals.resolve(
                        &id,
                        match verdict {
                            ApprovalVerdict::Approved => WriteApprovalDecision::Approved,
                            ApprovalVerdict::Denied => WriteApprovalDecision::Denied,
                        },
                    ),
                };
                (id, answered)
            }
        };
        if !answered {
            warn!(
                request = %id,
                "The broker answered a question this worker is no longer asking"
            );
        }
    }
}

/// An approval as the worker asks the root for it (EN-2a). The asker is the
/// broker's to stamp, so none is sent.
fn approval_request(detail: &ApprovalDetail) -> ApprovalRequest {
    ApprovalRequest {
        kind: match detail.kind {
            approvals::ApprovalKind::Exec => ApprovalKind::Exec,
            approvals::ApprovalKind::Write => ApprovalKind::Write,
        },
        command_or_path: detail.command_or_path.clone(),
        diff_stat: detail.diff_stat.clone(),
        asker: None,
    }
}

/// The wire's answers in the clarification store's vocabulary.
pub fn clarification_answers(answers: Vec<Answer>) -> Vec<ClarificationAnswer> {
    answers
        .into_iter()
        .map(|a| ClarificationAnswer {
            id: a.id,
            answer: a.answer,
            custom: a.custom,
        })
        .collect()
}

// ── AGE-467: the worker's tool-call trace ───────────────────────────────────
//
// A leader that delegates the same task to several workers and wants to
// *judge* their derivations, not just count their answers, needs to see what
// each worker did. `TaskMapper` already sees every tool call a worker makes
// (that is how the progress lines above are built); this accumulates the
// same events into a compacted trace, kept separate from the rendered wire
// string so a later structured export (e.g. an ATIF trajectory) can read the
// steps without touching `metadata["trace"]`'s shape.

/// One tool call as a judge would want to see it: what the worker ran, what
/// it was asked to run with, and how it came out.
#[derive(Debug, Clone)]
struct TraceStep {
    name: String,
    arguments: String,
    outcome: StepOutcome,
}

#[derive(Debug, Clone)]
enum StepOutcome {
    Ok(String),
    Failed(String),
    /// The call started but the worker never reported a result or error —
    /// it died mid-call.
    NoResult,
}

/// A traced tool call's `input` is cut at this many characters.
const TRACE_INPUT_CAP: usize = 2000;
/// A traced tool call's `output`/`error` is cut at this many characters.
const TRACE_OUTPUT_CAP: usize = 1200;
/// At most this many steps are kept; beyond it the middle is summarised by
/// one line — the first [`TRACE_HEAD_STEPS`] and the last [`TRACE_TAIL_STEPS`].
const TRACE_MAX_STEPS: usize = 40;
/// How many of the earliest steps survive the step-count cap.
const TRACE_HEAD_STEPS: usize = 2;
/// How many of the most recent steps survive the step-count cap.
const TRACE_TAIL_STEPS: usize = TRACE_MAX_STEPS - TRACE_HEAD_STEPS;
/// The whole trace is cut to this many characters, dropping steps from the
/// middle when the step-count cap alone still leaves it too big.
const TRACE_MAX_CHARS: usize = 12_000;

/// Cut `s` to `limit` characters, noting how much was removed. A cut always
/// gets its own line, so the marker never runs into the content it follows.
fn cap(s: &str, limit: usize) -> String {
    let total = s.chars().count();
    if total <= limit {
        return s.to_string();
    }
    let kept: String = s.chars().take(limit).collect();
    format!("{kept}\n\u{2026}[truncated {} chars]", total - limit)
}

/// One step's rendered block: `### name (ok|FAILED|no result)`, its input,
/// and its output or error.
fn render_step(step: &TraceStep) -> String {
    let input = cap(&step.arguments, TRACE_INPUT_CAP);
    match &step.outcome {
        StepOutcome::Ok(result) => format!(
            "### {} (ok)\ninput: {input}\noutput: {}",
            step.name,
            cap(result, TRACE_OUTPUT_CAP)
        ),
        StepOutcome::Failed(error) => format!(
            "### {} (FAILED)\ninput: {input}\nerror: {}",
            step.name,
            cap(error, TRACE_OUTPUT_CAP)
        ),
        StepOutcome::NoResult => format!("### {} (no result)\ninput: {input}", step.name),
    }
}

/// Render and compact `steps` into the wire's trace string: a port of
/// `/tmp/age9-judge/compact.py`'s global cap, with this issue's uniform
/// per-field caps rather than per-tool ones. `None` for a task with no tool
/// calls, so `terminal()` attaches no `trace` key at all.
fn compact_trace(steps: &[TraceStep]) -> Option<String> {
    if steps.is_empty() {
        return None;
    }

    let blocks: Vec<String> = steps.iter().map(render_step).collect();

    let head_n = TRACE_HEAD_STEPS.min(blocks.len());
    let head = &blocks[..head_n];
    let mut tail = &blocks[head_n..];
    if blocks.len() > TRACE_MAX_STEPS {
        tail = &blocks[blocks.len() - TRACE_TAIL_STEPS..];
    }

    // Whole-trace cap: drop further from the oldest end of what step-count
    // capping kept, preserving the head and as many of the most recent steps
    // as fit.
    let head_chars: usize = head.iter().map(|b| b.chars().count()).sum();
    let tail_chars: usize = tail.iter().map(|b| b.chars().count()).sum();
    if head_chars + tail_chars > TRACE_MAX_CHARS {
        let mut budget = TRACE_MAX_CHARS.saturating_sub(head_chars);
        let mut start = tail.len();
        for block in tail.iter().rev() {
            let len = block.chars().count();
            if len > budget {
                break;
            }
            budget -= len;
            start -= 1;
        }
        tail = &tail[start..];
    }

    let dropped = blocks.len() - head.len() - tail.len();
    let mut rendered: Vec<String> = head.to_vec();
    if dropped > 0 {
        rendered.push(format!("\u{2026}[{dropped} intermediate steps omitted]"));
    }
    rendered.extend(tail.iter().cloned());
    Some(rendered.join("\n"))
}

/// Folds one delegated task's events into frames.
pub struct TaskMapper {
    task_id: String,
    /// Tool call id → name, until its result arrives.
    tool_names: HashMap<String, String>,
    state: TaskState,
    failure: Option<String>,
    /// The typed failure of a worker under this one that never started
    /// (AGE-822): when set, the terminal status fails with it.
    start_failure: Option<String>,
    /// What this task spent, its own turns and everything its delegations
    /// reported (AGE-415), one line per model (AGE-682).
    usage: Vec<TokenUsage>,
    /// This task's tool calls, in the order they started (AGE-467).
    trace: Vec<TraceStep>,
    /// Tool call id → index into `trace`, for a call still waiting on its
    /// result or error.
    open_calls: HashMap<String, usize>,
    /// Whether this task's conversation is captured at its terminal status
    /// (RC-0, AGE-649). Off by default.
    capture_conversation: bool,
    /// Every `TurnMessages` event's messages, in arrival order, when capture
    /// is on. Untouched otherwise, so it never allocates for the common
    /// case.
    captured_messages: Vec<Value>,
    /// The schema the task's final answer must match (TD-2).
    handoff: Option<HandoffContract>,
    /// The text of the latest model call, since its last tool event: what
    /// the handoff is read from. Kept only when there is a schema.
    final_text: String,
    /// The handoff re-prompts the runner sent, each an invalid answer.
    handoff_follow_ups: u32,
    /// Whether the broker asked for `event` frames (TB-1). Off by default.
    swarm_events: bool,
    /// The approvals sent as `human.approve` and not yet answered, by the
    /// number this mapper gave them: the store's id and which store.
    approvals: HashMap<u64, (String, ApprovalKind)>,
    /// The last approval number given out.
    last_approval: u64,
    /// The questions sent as `human.ask` and not yet answered, by the
    /// number they were sent under: the clarification store's id.
    questions: HashMap<u64, String>,
    /// Every question this worker asks is numbered from this counter,
    /// shared with the worker's transport (EN-2b).
    question_numbers: Arc<AtomicU64>,
}

impl TaskMapper {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            tool_names: HashMap::new(),
            // A turn that emits nothing but `TurnEnded` completed; the
            // events that mean otherwise say so explicitly.
            state: TaskState::Completed,
            failure: None,
            start_failure: None,
            usage: Vec::new(),
            trace: Vec::new(),
            open_calls: HashMap::new(),
            capture_conversation: false,
            captured_messages: Vec::new(),
            handoff: None,
            final_text: String::new(),
            handoff_follow_ups: 0,
            swarm_events: false,
            approvals: HashMap::new(),
            last_approval: 0,
            questions: HashMap::new(),
            question_numbers: Arc::default(),
        }
    }

    /// Number this task's questions from `numbers`, the counter the
    /// worker's transport numbers the questions it relays from (EN-2b), so
    /// the two never collide on the connection.
    pub fn with_question_numbers(mut self, numbers: Arc<AtomicU64>) -> Self {
        self.question_numbers = numbers;
        self
    }

    /// The question whose answers just came back (EN-2b): its id on the
    /// clarification store, taken out. `None` for a number this mapper did
    /// not give out — a question the transport relayed.
    pub fn take_question(&mut self, question: u64) -> Option<String> {
        self.questions.remove(&question)
    }

    /// The approval the root just answered (EN-2a): the store's id and
    /// which store, taken out so that its resolution withdraws nothing.
    /// `None` for an approval this task no longer waits on.
    pub fn take_approval(&mut self, approval: u64) -> Option<(String, ApprovalKind)> {
        self.approvals.remove(&approval)
    }

    /// The `req.cancel` for an approval the worker's store let go of with
    /// no answer from the root — its own timeout, a cancelled turn — or
    /// `None`.
    pub fn withdrawn_approval(&mut self, event: &SessionEvent) -> Option<ParticipantFrame> {
        let SessionEvent::ApprovalResolved { id, .. } = event else {
            return None;
        };
        let approval = self
            .approvals
            .iter()
            .find(|(_, (store_id, _))| store_id == id)
            .map(|(approval, _)| *approval)?;
        self.approvals.remove(&approval);
        Some(ParticipantFrame::CancelApproval { id: approval })
    }

    /// Report this task's turns and tool events as `event` frames (TB-1).
    pub fn with_swarm_events(mut self, swarm_events: bool) -> Self {
        self.swarm_events = swarm_events;
        self
    }

    /// The `event` frame for `event`, when the broker asked for them and
    /// `event` is one of the worker's own turns or tool events (TB-1).
    pub fn swarm_event(&self, event: &SessionEvent) -> Option<ParticipantFrame> {
        if !self.swarm_events {
            return None;
        }
        let item = match event {
            SessionEvent::TurnStarted => WorkerSwarmItem::TurnStarted,
            SessionEvent::ToolCallStarted { id, name } => WorkerSwarmItem::ToolCallStarted {
                id: id.clone(),
                name: name.clone(),
            },
            SessionEvent::ToolCallResult { id, result } => WorkerSwarmItem::ToolCallResult {
                id: id.clone(),
                result: result.clone(),
            },
            SessionEvent::ToolCallError { id, error } => WorkerSwarmItem::ToolCallError {
                id: id.clone(),
                error: error.clone(),
            },
            _ => return None,
        };
        Some(ParticipantFrame::Event {
            task_id: self.task_id.clone(),
            event: item,
        })
    }

    /// Check the task's final answer against `handoff` (TD-2, AGE-693).
    /// `None`, the default, keeps the frames exactly what they were.
    pub fn with_handoff(mut self, handoff: Option<HandoffContract>) -> Self {
        self.handoff = handoff;
        self
    }

    /// Follow the last model call's text and count the handoff re-prompts,
    /// when the task has a schema.
    fn record_handoff_event(&mut self, event: &SessionEvent) {
        if self.handoff.is_none() {
            return;
        }
        match event {
            SessionEvent::Text(text) => self.final_text.push_str(text),
            SessionEvent::TurnStarted
            | SessionEvent::ToolCallStarted { .. }
            | SessionEvent::ToolCallResult { .. }
            | SessionEvent::ToolCallError { .. } => self.final_text.clear(),
            SessionEvent::FollowUp(prompt) if handoff::is_follow_up(prompt) => {
                self.handoff_follow_ups += 1;
            }
            _ => {}
        }
    }

    /// How the final answer measured up, for a task with a schema that
    /// completed. A failed or cancelled task is not read.
    fn handoff_outcome(&self) -> Option<HandoffOutcome> {
        let contract = self.handoff.as_ref()?;
        (self.state == TaskState::Completed).then(|| handoff::check(contract, &self.final_text))
    }

    /// Turn on conversation capture for this task (RC-0, AGE-649). Off by
    /// default: leaving this unset produces the exact frames this mapper
    /// always did (`capture_off_is_byte_identical`).
    pub fn with_capture_conversation(mut self, capture: bool) -> Self {
        self.capture_conversation = capture;
        self
    }

    /// Fold a tool-call event into the trace, if it is one (AGE-467). `Text`
    /// fragments are not traced — the response already carries them.
    fn record_trace_event(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::ToolCallStarted { id, name } => {
                self.open_calls.insert(id.clone(), self.trace.len());
                self.trace.push(TraceStep {
                    name: name.clone(),
                    arguments: String::new(),
                    outcome: StepOutcome::NoResult,
                });
            }
            SessionEvent::ToolCallInput { id, arguments } => {
                if let Some(&index) = self.open_calls.get(id) {
                    self.trace[index].arguments = arguments.clone();
                }
            }
            SessionEvent::ToolCallResult { id, result } => {
                if let Some(index) = self.open_calls.remove(id) {
                    self.trace[index].outcome = StepOutcome::Ok(result.clone());
                }
            }
            SessionEvent::ToolCallError { id, error } => {
                if let Some(index) = self.open_calls.remove(id) {
                    self.trace[index].outcome = StepOutcome::Failed(error.clone());
                }
                // A worker under this one never started: this task fails
                // with it, so the caller stops too and the user is shown
                // it (AGE-822), whatever the run then says.
                if self.start_failure.is_none()
                    && let Some((agent, reason)) = chatty_fabric::find_worker_start_failure(error)
                {
                    self.start_failure = Some(chatty_fabric::worker_start_failed(agent, reason));
                }
            }
            _ => {}
        }
    }

    /// The frame for `event`, or `None` for the events that stay in the child.
    pub fn map(&mut self, event: &SessionEvent) -> Option<ParticipantFrame> {
        self.record_trace_event(event);
        self.record_handoff_event(event);
        match event {
            SessionEvent::TurnStarted => Some(self.status(TaskState::Working, None)),

            SessionEvent::Text(text) => Some(ParticipantFrame::Artifact {
                task_id: self.task_id.clone(),
                text: text.clone(),
                last_chunk: false,
            }),

            // To the root, and only the root (EN-2a).
            SessionEvent::ApprovalRequested { id, detail, .. } => {
                self.last_approval += 1;
                let approval = self.last_approval;
                let kind = match detail.kind {
                    approvals::ApprovalKind::Exec => ApprovalKind::Exec,
                    approvals::ApprovalKind::Write => ApprovalKind::Write,
                };
                self.approvals.insert(approval, (id.clone(), kind));
                Some(ParticipantFrame::Approve {
                    id: approval,
                    request: approval_request(detail),
                })
            }
            // Up the caller chain (EN-2b), with no asker and no origin.
            SessionEvent::ClarificationRequested { id, questions } => {
                let question = self.question_numbers.fetch_add(1, Ordering::Relaxed) + 1;
                self.questions.insert(question, id.clone());
                Some(ParticipantFrame::Ask {
                    id: question,
                    request: AskRequest {
                        questions: questions
                            .iter()
                            .map(|q| Question {
                                id: q.id.clone(),
                                question: q.question.clone(),
                                options: q.options.clone(),
                            })
                            .collect(),
                        asker: None,
                        origin: None,
                    },
                })
            }

            // Usage is held rather than sent: see the module docs. A turn's
            // requests can name more than one model (a compaction's summary
            // call, AGE-683), so each goes on at its own. A plugin's
            // `llm::complete` calls are this task's spend too (PL-U2).
            SessionEvent::TokenUsage(usage) | SessionEvent::PluginUsage(usage) => {
                if usage.calls.is_empty() {
                    merge_line(&mut self.usage, usage.clone());
                } else {
                    for call in &usage.calls {
                        let mut line = TokenUsage::from_calls(vec![call.clone()]);
                        line.model = call.model.clone().or_else(|| usage.model.clone());
                        line.calls.clear();
                        merge_line(&mut self.usage, line);
                    }
                }
                None
            }
            // A grandchild's spend is forwarded with this task's, each line
            // under the path to the agent that spent it (TB-3).
            SessionEvent::Delegation(InvokeAgentProgress::Finished { usage, .. })
                if !usage.is_empty() =>
            {
                for line in usage {
                    merge_line(&mut self.usage, line.clone());
                }
                None
            }

            // Held rather than sent, like usage, and only when capture is on
            // (RC-0, AGE-649): see the module docs.
            SessionEvent::TurnMessages(messages) => {
                if self.capture_conversation
                    && let Ok(Value::Array(items)) = serde_json::to_value(messages)
                {
                    self.captured_messages.extend(items);
                }
                None
            }

            SessionEvent::Cancelled => {
                self.state = TaskState::Canceled;
                None
            }
            SessionEvent::Error(error) => {
                self.state = TaskState::Failed;
                self.failure = Some(error.message.clone());
                // Still reported as progress: a failed turn is something
                // the parent's transcript should show, not just something
                // the terminal status records.
                progress_text_for_event(event, &mut self.tool_names)
                    .map(|text| self.status(TaskState::Working, Some(text)))
            }

            // Everything else that has a progress line: the tool events, and
            // a grandchild's progress.
            _ => progress_text_for_event(event, &mut self.tool_names)
                .map(|text| self.status(TaskState::Working, Some(text))),
        }
    }

    /// The single terminal status for the whole delegation.
    pub fn terminal(&self) -> ParticipantFrame {
        let handoff = self.handoff_outcome();
        let (state, message) = match (&handoff, self.handoff.as_ref()) {
            (Some(HandoffOutcome::Invalid { errors }), Some(contract)) => (
                TaskState::Failed,
                Some(format!(
                    "handoff_invalid: {}: {}",
                    contract.role,
                    errors.join("; ")
                )),
            ),
            _ => match &self.start_failure {
                Some(failure) => (TaskState::Failed, Some(failure.clone())),
                None => (self.state, self.failure.clone()),
            },
        };
        ParticipantFrame::Status {
            task_id: self.task_id.clone(),
            state,
            message,
            metadata: self.terminal_metadata(handoff),
        }
    }

    /// The handoff keys for the terminal status: the valid handoff, or the
    /// role and errors of an invalid one, and how many answers failed.
    fn handoff_metadata(&self, outcome: Option<HandoffOutcome>, metadata: &mut TaskMetadata) {
        let Some(contract) = self.handoff.as_ref() else {
            return;
        };
        let mut invalid = self.handoff_follow_ups;
        match outcome {
            Some(HandoffOutcome::Valid(value)) => {
                metadata.handoff = Opaque::from_value(&value)
                    .map_err(|e| warn!(error = %e, "The handoff does not fit the terminal status"))
                    .ok();
            }
            Some(HandoffOutcome::Invalid { errors }) => {
                invalid += 1;
                metadata.handoff_invalid = Some(HandoffInvalid {
                    role: contract.role.clone(),
                    errors,
                });
            }
            None => {}
        }
        if invalid > 0 {
            metadata.handoff_invalid_count = Some(invalid);
        }
    }

    /// Everything that rides on the terminal status's `metadata`: usage
    /// (ADR-0011) and, when this task made any tool calls, the compacted
    /// trace (AGE-467), the captured conversation (RC-0) and the handoff
    /// keys (TD-2). `None` when none has anything to report.
    fn terminal_metadata(&self, handoff: Option<HandoffOutcome>) -> Option<TaskMetadata> {
        let mut metadata = TaskMetadata {
            usage: (!self.usage.is_empty()).then(|| wire_usage(&self.usage)),
            trace: compact_trace(&self.trace),
            ..TaskMetadata::default()
        };
        self.conversation_metadata(&mut metadata);
        self.handoff_metadata(handoff, &mut metadata);
        (!metadata.is_empty()).then_some(metadata)
    }

    /// The captured conversation for [`terminal_metadata`](Self::terminal_metadata)
    /// (RC-0, AGE-649): nothing when capture was never turned on for this
    /// task. Above [`chatty_fabric::wire::OPAQUE_CAP_BYTES`] (the cap per
    /// `fabric-resumable-conversations` §4.1) the messages are replaced with
    /// their byte count, never silently truncated.
    fn conversation_metadata(&self, metadata: &mut TaskMetadata) {
        if !self.capture_conversation {
            return;
        }
        let messages = serde_json::to_string(&self.captured_messages);
        match messages.map(Opaque::from_json) {
            Ok(Ok(conversation)) => metadata.conversation = Some(conversation),
            Ok(Err(chatty_fabric::wire::OpaqueError::TooLarge(bytes))) => {
                metadata.conversation_too_large = Some(bytes as u64);
            }
            Ok(Err(e)) => warn!(error = %e, "The captured conversation is not JSON"),
            Err(e) => warn!(error = %e, "The captured conversation does not serialise"),
        }
    }

    fn status(&self, state: TaskState, message: Option<String>) -> ParticipantFrame {
        ParticipantFrame::Status {
            task_id: self.task_id.clone(),
            state,
            message,
            metadata: None,
        }
    }
}

/// Fold `line` into `lines`: onto the line with the same agent and model
/// when there is one, as a line of its own otherwise (AGE-682, TB-3). A
/// parent is told what was spent on which model, and by whom below it.
fn merge_line(lines: &mut Vec<TokenUsage>, line: TokenUsage) {
    let Some(total) = lines
        .iter_mut()
        .find(|known| known.model == line.model && known.delegated_to == line.delegated_to)
    else {
        lines.push(TokenUsage {
            plugin: None,
            calls: Vec::new(),
            estimated_cost_usd: None,
            ..line
        });
        return;
    };
    total.input_tokens = total.input_tokens.saturating_add(line.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(line.output_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(line.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(line.cache_write_tokens);
    total.api_turn_count = total.api_turn_count.saturating_add(line.api_turn_count);
    total.duration_ms = total.duration_ms.saturating_add(line.duration_ms);
    total.at = total.at.max(line.at);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::models::token_usage::{ApiCallUsage, ModelRef};
    use chatty_core::services::{StreamError, StreamErrorKind};
    use chatty_core::settings::models::providers_store::ProviderType;
    use rig_core::completion::Message;

    /// How the task would end if its turns stopped now.
    fn outcome(mapper: &TaskMapper) -> TaskState {
        status_of(&mapper.terminal()).0
    }

    fn status_of(frame: &ParticipantFrame) -> (TaskState, Option<String>) {
        match frame {
            ParticipantFrame::Status { state, message, .. } => (*state, message.clone()),
            other => panic!("expected a status frame, got {other:?}"),
        }
    }

    fn tool_started(id: &str, name: &str) -> SessionEvent {
        SessionEvent::ToolCallStarted {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn a_tool_round_trip_becomes_two_progress_statuses() {
        let mut mapper = TaskMapper::new("task-1");

        let started = mapper.map(&tool_started("c1", "read_file")).unwrap();
        assert_eq!(
            status_of(&started),
            (TaskState::Working, Some("read_file".to_string()))
        );

        let done = mapper
            .map(&SessionEvent::ToolCallResult {
                id: "c1".into(),
                result: "# Chatty".into(),
            })
            .unwrap();
        assert_eq!(
            status_of(&done),
            (TaskState::Working, Some("\u{2713} read_file".to_string()))
        );
    }

    #[test]
    fn a_failing_tool_is_marked_but_does_not_fail_the_task() {
        let mut mapper = TaskMapper::new("task-1");
        mapper.map(&tool_started("c1", "shell"));
        let errored = mapper
            .map(&SessionEvent::ToolCallError {
                id: "c1".into(),
                error: "exit 1".into(),
            })
            .unwrap();

        assert_eq!(
            status_of(&errored),
            (TaskState::Working, Some("\u{2717} shell".to_string()))
        );
        assert_eq!(
            outcome(&mapper),
            TaskState::Completed,
            "a tool may fail and the turn still succeed; only a stream error ends the task"
        );
    }

    #[test]
    fn text_becomes_a_non_final_artifact() {
        let mut mapper = TaskMapper::new("task-1");
        let frame = mapper.map(&SessionEvent::Text("Hello".into())).unwrap();
        let ParticipantFrame::Artifact {
            task_id,
            text,
            last_chunk,
        } = frame
        else {
            panic!("text is an artifact, not a status");
        };
        assert_eq!(task_id, "task-1");
        assert_eq!(text, "Hello");
        assert!(!last_chunk, "more text may follow");
    }

    #[test]
    fn turn_ended_emits_nothing_because_one_task_can_span_several_turns() {
        let mut mapper = TaskMapper::new("task-1");
        assert!(mapper.map(&SessionEvent::TurnEnded).is_none());
        assert!(
            mapper
                .map(&SessionEvent::FollowUp("again".into()))
                .is_none()
        );
        assert!(
            mapper
                .map(&SessionEvent::TurnMessages(Vec::new()))
                .is_none()
        );
        assert!(
            mapper
                .map(&SessionEvent::ToolCallInput {
                    id: "c1".into(),
                    arguments: "{}".into(),
                })
                .is_none(),
            "the parent renders tool names, not their arguments"
        );
    }

    #[test]
    fn a_stream_error_fails_the_task_and_still_shows_as_progress() {
        let mut mapper = TaskMapper::new("task-1");
        let frame = mapper
            .map(&SessionEvent::Error(StreamError {
                kind: StreamErrorKind::Other,
                message: "the provider hung up".into(),
            }))
            .unwrap();

        assert_eq!(
            status_of(&frame),
            (
                TaskState::Working,
                Some("error: the provider hung up".to_string())
            )
        );
        assert_eq!(outcome(&mapper), TaskState::Failed);
        let (state, message) = status_of(&mapper.terminal());
        assert_eq!(state, TaskState::Failed);
        assert_eq!(message.as_deref(), Some("the provider hung up"));
    }

    #[test]
    fn cancellation_is_its_own_terminal_state() {
        let mut mapper = TaskMapper::new("task-1");
        assert!(mapper.map(&SessionEvent::Cancelled).is_none());
        assert_eq!(outcome(&mapper), TaskState::Canceled);
    }

    fn model(id: &str) -> ModelRef {
        ModelRef {
            provider: ProviderType::OpenRouter,
            model_id: id.to_string(),
        }
    }

    /// A turn's usage as the session reports it: one call on `model`.
    fn turn_on(id: &str, input: u32, output: u32) -> TokenUsage {
        TokenUsage::from_calls(vec![ApiCallUsage {
            turn: 1,
            input_tokens: input,
            output_tokens: output,
            model: Some(model(id)),
            ..Default::default()
        }])
    }

    fn finished(usage: Vec<TokenUsage>) -> SessionEvent {
        SessionEvent::Delegation(InvokeAgentProgress::Finished {
            success: true,
            result: None,
            usage,
        })
    }

    fn terminal_metadata(mapper: &TaskMapper) -> Value {
        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        wire(metadata.expect("usage is attached to the terminal status"))
    }

    /// Terminal metadata as it is on the wire.
    fn wire(metadata: TaskMetadata) -> Value {
        serde_json::to_value(metadata).expect("metadata serialises")
    }

    #[test]
    fn usage_rides_in_the_terminal_status_metadata() {
        let mut mapper = TaskMapper::new("task-1");
        assert!(
            mapper
                .map(&SessionEvent::TokenUsage(turn_on("worker", 120, 34)))
                .is_none(),
            "usage is not a task event"
        );

        let metadata = terminal_metadata(&mapper);
        assert_eq!(metadata["usage"]["inputTokens"], 120);
        assert_eq!(metadata["usage"]["outputTokens"], 34);
        // The leader reads it back with core's reader (AGE-415): the two
        // spellings are pinned to each other here.
        let read = chatty_core::services::a2a_client::usage_from_status_metadata(Some(&metadata));
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].input_tokens, read[0].output_tokens), (120, 34));
        assert_eq!(read[0].model, Some(model("worker")));
    }

    /// AGE-682: nothing on the wire is a price. Each line names its model
    /// and carries its tokens; whoever reads it prices it.
    #[test]
    fn wire_usage_has_no_price() {
        let mut mapper = TaskMapper::new("task-1");
        let mut own = turn_on("worker", 1_000_000, 0);
        // Even a line that was priced somewhere does not send its price.
        own.estimated_cost_usd = Some(15.0);
        mapper.map(&SessionEvent::TokenUsage(own));

        let usage = &terminal_metadata(&mapper)["usage"];
        let lines = usage["lines"].as_array().expect("lines");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["model"]["model_id"], "worker");
        assert_eq!(lines[0]["model"]["provider"], "open_router");
        assert_eq!(lines[0]["inputTokens"], 1_000_000);
        let text = usage.to_string().to_lowercase();
        for priced in ["cost", "price", "usd"] {
            assert!(!text.contains(priced), "{priced} on the wire: {usage}");
        }
    }

    /// AGE-682: a three-level run on three models. Every level forwards
    /// lines rather than a pre-priced number, and lines merge only when they
    /// share a model, so the root has one line per model and its total is
    /// each line priced at its own model.
    #[test]
    fn nested_lines_keep_their_model() {
        use chatty_core::models::token_usage::{PriceBook, TokenPricing, price};
        use chatty_core::services::a2a_client::usage_from_status_metadata;

        // Level 3: two workers on model C (one also ran a compaction on B).
        let leaf = |input: u32| {
            let mut mapper = TaskMapper::new("leaf");
            let mut usage = TokenUsage::from_calls(vec![
                ApiCallUsage {
                    input_tokens: 10,
                    model: Some(model("b")),
                    ..Default::default()
                },
                ApiCallUsage {
                    turn: 1,
                    input_tokens: input,
                    model: Some(model("c")),
                    ..Default::default()
                },
            ]);
            usage.model = Some(model("c"));
            mapper.map(&SessionEvent::TokenUsage(usage));
            usage_from_status_metadata(Some(&terminal_metadata(&mapper)))
        };
        let (first, second) = (leaf(1_000), leaf(2_000));
        assert_eq!(first.len(), 2, "one line per model: {first:?}");

        // Level 2: a sub-leader on model B, over both leaves, across two
        // of its own turns.
        let mut sub = TaskMapper::new("sub");
        sub.map(&finished(first));
        sub.map(&finished(second));
        sub.map(&SessionEvent::TokenUsage(turn_on("b", 100, 0)));
        sub.map(&SessionEvent::TokenUsage(turn_on("b", 200, 0)));
        let reported = usage_from_status_metadata(Some(&terminal_metadata(&sub)));

        // Level 1: the root on model A.
        let mut root = reported;
        root.push(turn_on("a", 5, 0));

        let models: Vec<&str> = root
            .iter()
            .map(|line| line.model.as_ref().unwrap().model_id.as_str())
            .collect();
        assert_eq!(models, ["b", "c", "a"], "one line per model");
        let tokens = |id: &str| {
            root.iter()
                .find(|l| l.model.as_ref().unwrap().model_id == id)
                .unwrap()
                .input_tokens
        };
        assert_eq!(tokens("c"), 3_000);
        assert_eq!(tokens("b"), 10 + 10 + 100 + 200);

        let mut book = PriceBook::default();
        for (id, rate) in [("a", 1.0), ("b", 3.0), ("c", 15.0)] {
            book.insert(
                model(id),
                TokenPricing {
                    input_per_million: rate,
                    ..Default::default()
                },
            );
        }
        let cost = price(&root, &book);
        assert_eq!(cost.unpriced_lines, 0);
        let expected = (5.0 * 1.0 + 320.0 * 3.0 + 3_000.0 * 15.0) / 1_000_000.0;
        assert!(
            (cost.usd - expected).abs() < 1e-12,
            "{} vs {expected}",
            cost.usd
        );
    }

    /// A worker whose own turn reported no usage still passes on what it
    /// delegated, rather than dropping the subtree's spend.
    #[test]
    fn delegated_usage_alone_still_reaches_the_terminal_status() {
        let mut mapper = TaskMapper::new("task-1");
        mapper.map(&SessionEvent::Delegation(InvokeAgentProgress::Finished {
            success: false,
            result: None,
            usage: vec![TokenUsage {
                model: Some(model("worker")),
                ..TokenUsage::new(7, 3)
            }],
        }));
        let metadata = terminal_metadata(&mapper);
        assert_eq!(metadata["usage"]["inputTokens"], 7);
        assert_eq!(metadata["usage"]["outputTokens"], 3);
    }

    /// A delegation that reported nothing (a WASM module, say) is still a
    /// progress line, and adds no usage.
    #[test]
    fn a_delegation_without_usage_adds_none() {
        let mut mapper = TaskMapper::new("task-1");
        mapper.map(&finished(Vec::new()));
        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        assert!(metadata.is_none(), "{metadata:?}");
    }

    /// EN-2b: a question is a numbered `human.ask` request carrying every
    /// question, with no asker or origin of its own, and the number maps
    /// back to the store's request id.
    #[test]
    fn a_question_parks_the_task_with_everything_needed_to_answer_it() {
        use chatty_core::models::clarification_store::ClarifyingQuestion;

        let mut mapper = TaskMapper::new("task-1");
        let frame = mapper
            .map(&SessionEvent::ClarificationRequested {
                id: "req-1".into(),
                questions: vec![
                    ClarifyingQuestion {
                        id: "q1".into(),
                        question: "Which database?".into(),
                        options: vec!["Postgres".into(), "SQLite".into()],
                    },
                    ClarifyingQuestion {
                        id: "q2".into(),
                        question: "Which region?".into(),
                        options: vec!["eu".into(), "us".into()],
                    },
                ],
            })
            .unwrap();
        let ParticipantFrame::Ask { id, request } = frame else {
            panic!("a question is a human.ask request: {frame:?}");
        };
        assert_eq!(
            request.questions.len(),
            2,
            "every question goes up, not just the first"
        );
        assert_eq!(request.questions[1].options, vec!["eu", "us"]);
        assert_eq!((request.asker, request.origin), (None, None));
        assert_eq!(
            mapper.take_question(id).as_deref(),
            Some("req-1"),
            "the store's request id is what the answers resolve"
        );
        assert_eq!(outcome(&mapper), TaskState::Completed, "parked, not over");
    }

    #[tokio::test]
    async fn answers_from_the_broker_reach_the_waiting_tool() {
        use chatty_core::models::clarification_store::{ClarifyingQuestion, request_clarification};

        let mut store = ClarificationStore::new();
        let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
        store.set_notifier(notify_tx);
        let pending = store.get_pending_clarifications();
        let waiter = tokio::spawn(async move {
            request_clarification(
                &pending,
                vec![ClarifyingQuestion {
                    id: "q1".into(),
                    question: "Which database?".into(),
                    options: vec!["Postgres".into(), "SQLite".into()],
                }],
            )
            .await
        });
        // The store announces the request the way it announces it to a
        // frontend; its id is what the broker's answer must name.
        let request_id = notify_rx.recv().await.expect("the request is announced").id;

        let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
        tokio::spawn(answer_inputs(
            inputs_rx,
            InputStores {
                clarifications: store,
                execution_approvals: ExecutionApprovalStore::new(),
                write_approvals: WriteApprovalStore::new(),
            },
        ));
        inputs_tx
            .send(WorkerInput::Answer {
                id: request_id,
                answers: vec![Answer {
                    id: "q1".into(),
                    answer: "SQLite".into(),
                    custom: false,
                }],
            })
            .unwrap();

        let answers = waiter.await.unwrap().unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].id, "q1");
        assert_eq!(answers[0].answer, "SQLite");
        assert!(!answers[0].custom);
    }

    /// EN-2a: an approval is a `human.approve` request for the root, not a
    /// parked status for the caller; the task is not parked toward anyone.
    #[test]
    fn a_blocked_tool_asks_the_root_rather_than_its_caller() {
        let mut mapper = TaskMapper::new("task-1");
        let frame = mapper
            .map(&SessionEvent::ApprovalRequested {
                id: "a1".into(),
                command: "[shell] rm -rf /".into(),
                is_sandboxed: false,
                detail: ApprovalDetail::exec("[shell] rm -rf /"),
            })
            .unwrap();
        let ParticipantFrame::Approve { id, request } = frame else {
            panic!("an approval is a human.approve request: {frame:?}");
        };
        assert_eq!(
            request,
            ApprovalRequest {
                kind: ApprovalKind::Exec,
                command_or_path: "[shell] rm -rf /".into(),
                diff_stat: None,
                asker: None,
            }
        );
        assert_eq!(outcome(&mapper), TaskState::Completed);

        // Answered by the root: taken out, so its resolution withdraws
        // nothing.
        assert_eq!(
            mapper.take_approval(id),
            Some(("a1".to_string(), ApprovalKind::Exec))
        );
        let resolved = SessionEvent::ApprovalResolved {
            id: "a1".into(),
            approved: true,
        };
        assert!(mapper.withdrawn_approval(&resolved).is_none());

        // Let go of unanswered: withdrawn.
        let Some(ParticipantFrame::Approve { id: second, .. }) =
            mapper.map(&SessionEvent::ApprovalRequested {
                id: "a2".into(),
                command: "[shell] ls".into(),
                is_sandboxed: false,
                detail: ApprovalDetail::exec("[shell] ls"),
            })
        else {
            panic!("a second approval");
        };
        assert_ne!(second, id, "each approval gets its own number");
        let timed_out = SessionEvent::ApprovalResolved {
            id: "a2".into(),
            approved: false,
        };
        assert!(matches!(
            mapper.withdrawn_approval(&timed_out),
            Some(ParticipantFrame::CancelApproval { id }) if id == second
        ));
    }

    /// EN-2a: an answer to a question resolves only the clarification
    /// store. One that names a pending approval's id and reads `approve`
    /// grants nothing; the root's verdict, on the store it was raised on,
    /// is the only thing that does.
    #[tokio::test]
    async fn human_ask_answered_approve_grants_nothing() {
        use chatty_core::models::execution_approval_store::request_execution_approval;
        use chatty_core::settings::models::execution_settings::ApprovalMode;

        let mut execution = ExecutionApprovalStore::new();
        let (raised_tx, mut raised) = mpsc::unbounded_channel();
        let (resolved_tx, _resolved) = mpsc::unbounded_channel();
        execution.set_notifiers(raised_tx, resolved_tx);
        let pending = execution.get_pending_approvals();
        let waiter = tokio::spawn(async move {
            request_execution_approval(
                &pending,
                &ApprovalMode::AlwaysAsk,
                "[shell] rm -rf /",
                false,
            )
            .await
        });
        let approval = raised.recv().await.expect("the approval is raised").id;

        let stores = InputStores {
            clarifications: ClarificationStore::new(),
            execution_approvals: execution.clone(),
            write_approvals: WriteApprovalStore::new(),
        };
        // A question's answer naming the approval and reading `approve`,
        // the way the old relay granted one. The loop has handled it once
        // it returns, which it does when the channel closes.
        let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
        inputs_tx
            .send(WorkerInput::Answer {
                id: approval.clone(),
                answers: vec![Answer {
                    id: approval.clone(),
                    answer: "approve".into(),
                    custom: false,
                }],
            })
            .unwrap();
        drop(inputs_tx);
        answer_inputs(inputs_rx, stores.clone()).await;
        assert_eq!(
            execution.pending_ids(),
            std::slice::from_ref(&approval),
            "the approval is still waiting"
        );
        assert!(!waiter.is_finished(), "nothing was granted");

        // The root's verdict, on the store it was raised on, decides.
        let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
        inputs_tx
            .send(WorkerInput::Approval {
                id: approval,
                kind: ApprovalKind::Exec,
                verdict: ApprovalVerdict::Denied,
            })
            .unwrap();
        drop(inputs_tx);
        answer_inputs(inputs_rx, stores).await;
        assert!(!waiter.await.unwrap().unwrap(), "denied by the root");
    }

    // ── AGE-467: the worker's tool-call trace ───────────────────────────────

    /// The terminal metadata's `trace` string for a mapper, or a panic if the
    /// frame isn't a status.
    fn trace_of(mapper: &TaskMapper) -> Option<String> {
        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        metadata.and_then(|m| m.trace)
    }

    #[test]
    fn two_tool_round_trips_both_appear_in_the_trace_with_their_input_and_output() {
        let mut mapper = TaskMapper::new("task-1");
        mapper.map(&tool_started("c1", "read_file"));
        mapper.map(&SessionEvent::ToolCallInput {
            id: "c1".into(),
            arguments: r#"{"path":"README.md"}"#.into(),
        });
        mapper.map(&SessionEvent::ToolCallResult {
            id: "c1".into(),
            result: "# Chatty".into(),
        });
        mapper.map(&tool_started("c2", "write_file"));
        mapper.map(&SessionEvent::ToolCallInput {
            id: "c2".into(),
            arguments: r#"{"path":"out.txt"}"#.into(),
        });
        mapper.map(&SessionEvent::ToolCallResult {
            id: "c2".into(),
            result: "wrote 2 bytes".into(),
        });

        let trace = trace_of(&mapper).expect("two tool calls produce a trace");
        assert!(trace.contains("### read_file (ok)"), "{trace}");
        assert!(trace.contains(r#"input: {"path":"README.md"}"#), "{trace}");
        assert!(trace.contains("output: # Chatty"), "{trace}");
        assert!(trace.contains("### write_file (ok)"), "{trace}");
        assert!(trace.contains(r#"input: {"path":"out.txt"}"#), "{trace}");
        assert!(trace.contains("output: wrote 2 bytes"), "{trace}");
    }

    #[test]
    fn a_failing_tool_traces_as_failed_with_its_error() {
        let mut mapper = TaskMapper::new("task-1");
        mapper.map(&tool_started("c1", "shell"));
        mapper.map(&SessionEvent::ToolCallInput {
            id: "c1".into(),
            arguments: r#"{"command":"false"}"#.into(),
        });
        mapper.map(&SessionEvent::ToolCallError {
            id: "c1".into(),
            error: "exit 1".into(),
        });

        let trace = trace_of(&mapper).expect("a failed call still produces a trace");
        assert!(trace.contains("### shell (FAILED)"), "{trace}");
        assert!(trace.contains("error: exit 1"), "{trace}");
    }

    #[test]
    fn an_oversized_input_or_output_is_cut_with_a_truncation_marker() {
        let mut mapper = TaskMapper::new("task-1");
        let long_input = "a".repeat(2500);
        let long_output = "b".repeat(1500);
        mapper.map(&tool_started("c1", "search_web"));
        mapper.map(&SessionEvent::ToolCallInput {
            id: "c1".into(),
            arguments: long_input,
        });
        mapper.map(&SessionEvent::ToolCallResult {
            id: "c1".into(),
            result: long_output,
        });

        let trace = trace_of(&mapper).expect("the call produces a trace");
        assert!(
            trace.contains("\u{2026}[truncated 500 chars]"),
            "the input's overflow (2500 - 2000) is not reported: {trace}"
        );
        assert!(
            trace.contains("\u{2026}[truncated 300 chars]"),
            "the output's overflow (1500 - 1200) is not reported: {trace}"
        );
        assert!(
            !trace.contains(&"a".repeat(2001)),
            "input over the cap leaked through uncut"
        );
        assert!(
            !trace.contains(&"b".repeat(1201)),
            "output over the cap leaked through uncut"
        );
    }

    #[test]
    fn sixty_steps_are_compacted_to_forty_with_an_omission_line() {
        let mut mapper = TaskMapper::new("task-1");
        for i in 0..60 {
            let id = format!("c{i}");
            mapper.map(&tool_started(&id, "read_file"));
            mapper.map(&SessionEvent::ToolCallResult {
                id: id.clone(),
                result: format!("result {i}"),
            });
        }

        let trace = trace_of(&mapper).expect("sixty tool calls produce a trace");
        assert!(
            trace.contains("\u{2026}[20 intermediate steps omitted]"),
            "{trace}"
        );
        assert_eq!(
            trace.matches("### read_file").count(),
            40,
            "2 head + 38 tail steps should remain: {trace}"
        );
        assert!(trace.contains("output: result 0"), "the first step remains");
        assert!(
            trace.contains("output: result 1\n"),
            "the second step remains"
        );
        assert!(
            !trace.contains("output: result 2\n"),
            "the third step is inside the omitted middle: {trace}"
        );
        assert!(trace.contains("output: result 59"), "the last step remains");
    }

    /// A trace whose steps are individually within the field caps can still
    /// add up past the whole-trace cap; the middle gives way, not the ends.
    #[test]
    fn a_trace_over_the_whole_cap_drops_middle_steps_and_stays_under_it() {
        let mut mapper = TaskMapper::new("task-1");
        // Four steps at ~3.2 KB each (a 2000-char input, a 1200-char output)
        // sum past TRACE_MAX_CHARS (12 000), so the char cap must trim what
        // the step-count cap (well under 40) would otherwise keep whole.
        for i in 0..4 {
            let id = format!("c{i}");
            mapper.map(&tool_started(&id, "read_file"));
            mapper.map(&SessionEvent::ToolCallInput {
                id: id.clone(),
                arguments: "x".repeat(2000),
            });
            mapper.map(&SessionEvent::ToolCallResult {
                id: id.clone(),
                result: format!("{}{}", "y".repeat(1199), i),
            });
        }

        let trace = trace_of(&mapper).expect("four tool calls produce a trace");
        assert!(
            trace.chars().count() <= 12_000,
            "the trace exceeds the whole-trace cap: {} chars",
            trace.chars().count()
        );
        assert!(
            trace.contains("intermediate steps omitted"),
            "some step had to be dropped for this to fit: {trace}"
        );
        // The most recent step is kept over an older one in the middle.
        assert!(
            trace.ends_with('3'),
            "the last step's output should survive: {trace}"
        );
        assert!(
            !trace.contains(&format!("{}{}", "y".repeat(1199), 2)),
            "a middle step should have been dropped: {trace}"
        );
    }

    #[test]
    fn a_task_with_no_tool_calls_attaches_no_trace_key() {
        let mut mapper = TaskMapper::new("task-1");
        mapper.map(&SessionEvent::TurnStarted);
        mapper.map(&SessionEvent::Text("hi".into()));
        mapper.map(&SessionEvent::TurnEnded);

        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        assert!(
            metadata.is_none(),
            "nothing to report: no usage, no trace: {metadata:?}"
        );
    }

    // ── RC-0 (AGE-649): the worker's captured conversation ───────────────────

    /// AGE-247's order: the prompt, an assistant tool call, its tool result,
    /// then the final text — rig's own record of the turn, which
    /// `SessionEvent::TurnMessages` already carries whole. Capture just has
    /// to preserve it verbatim.
    #[test]
    fn capture_equals_worker_history() {
        let history = vec![
            Message::user("read the readme and summarise it"),
            Message::assistant("calling read_file"),
            Message::user("# Chatty\nA desktop chat app."),
            Message::assistant("It's a chat app."),
        ];

        let mut mapper = TaskMapper::new("task-1").with_capture_conversation(true);
        assert!(
            mapper
                .map(&SessionEvent::TurnMessages(history.clone()))
                .is_none(),
            "TurnMessages never becomes a frame, captured or not"
        );

        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        let metadata = wire(metadata.expect("the captured conversation is attached"));
        let expected = serde_json::to_value(&history).unwrap();
        assert_eq!(
            metadata["conversation"], expected,
            "the captured conversation is the worker's own history, message for message"
        );
    }

    /// Above the 32 MB cap, the terminal status reports the byte count
    /// instead of the data (spec §4.1) — never a silently truncated
    /// conversation.
    #[test]
    fn capture_cap_enforced() {
        let huge = Message::assistant("x".repeat(chatty_fabric::wire::OPAQUE_CAP_BYTES + 1024));
        let mut mapper = TaskMapper::new("task-1").with_capture_conversation(true);
        assert!(
            mapper
                .map(&SessionEvent::TurnMessages(vec![huge]))
                .is_none()
        );

        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        let metadata = wire(metadata.expect("the cap being hit is itself reported"));
        assert!(
            metadata.get("conversation").is_none(),
            "an oversized conversation is never sent: {metadata}"
        );
        let bytes = metadata["conversationTooLarge"]
            .as_u64()
            .expect("the byte count is reported instead of the data");
        assert!(
            bytes as usize > chatty_fabric::wire::OPAQUE_CAP_BYTES,
            "the reported size should be what the data actually was: {bytes}"
        );
    }

    /// Capture is opt-in: a task that never turns it on puts nothing new on
    /// the wire, even when a `TurnMessages` event with real content flows
    /// through the mapper — the frames stay exactly what they were before
    /// this feature existed.
    #[test]
    fn capture_off_is_byte_identical() {
        let mut mapper = TaskMapper::new("task-1");
        assert!(
            mapper
                .map(&SessionEvent::TurnMessages(vec![Message::user(
                    "read the readme"
                )]))
                .is_none()
        );
        mapper.map(&tool_started("c1", "read_file"));
        mapper.map(&SessionEvent::ToolCallResult {
            id: "c1".into(),
            result: "# Chatty".into(),
        });

        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        let metadata = wire(metadata.expect("the tool call still produces a trace"));
        assert!(
            metadata.get("conversation").is_none(),
            "capture is off: no conversation on the wire: {metadata}"
        );
        assert!(metadata.get("conversationTooLarge").is_none());
        assert!(
            metadata["trace"]
                .as_str()
                .unwrap()
                .contains("### read_file (ok)"),
            "everything else is exactly what it was before this feature existed: {metadata}"
        );
    }
}
