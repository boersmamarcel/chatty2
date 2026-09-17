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
//! | `ApprovalRequested` | `status: input-required` |
//! | `ClarificationRequested` | `status: input-required`, carrying the request (see below) |
//! | `ApprovalResolved` | `status: working` |
//! | `Delegation` | `status: working` (a grandchild's progress) |
//! | `ApiCallUsage` | — (folded into `TokenUsage`) |
//! | `TokenUsage` | — (held, and attached to the terminal status) |
//! | `TurnMessages` | — |
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
//! Its status frame carries the whole request — the store's request id and
//! every question with its options — so the caller can put the same popover
//! in front of a human, or park its own task the same way if it is a worker
//! too (ADR-0011 C7, AGE-306). The answer comes back as a
//! [`TaskInput`](crate::participant::TaskInput) and is handed to the
//! worker's clarification store by [`answer_clarifications`]; the tool
//! result that follows is what un-parks the task.
//!
//! # What A2A cannot carry
//!
//! Token usage. A2A has no notion of it, and inventing a frame would put
//! accounting into the task protocol. It rides in the terminal status's
//! `metadata`, which is where ADR-0011's ledger (AGE-307) reads it. What
//! this worker's own delegations spent is folded into that number before
//! it goes (AGE-415), so a parent sees one number per delegation however
//! deep the tree below it, and the root's line carries the whole tree.

use crate::participant::{InputQuestion, InputRequest, ParticipantFrame, TaskInput, TaskState};
use chatty_core::models::clarification_store::{ClarificationAnswer, ClarificationStore};
use chatty_core::models::token_usage::TokenUsage;
use chatty_core::services::a2a_client::TRACE_METADATA_KEY;
use chatty_core::session::SessionEvent;
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_core::tools::progress_text_for_event;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

/// Answers for the running task, as the broker delivers them.
pub type InputReceiver = mpsc::UnboundedReceiver<TaskInput>;

/// Hand every answer the broker sends down to the store the worker's
/// `ask_user` is waiting on, until the task is over.
///
/// The embedder spawns this beside its turn: the answers arrive on the
/// socket's read half while the turn runs, and the store is the one thing
/// both the tool and this loop can reach.
pub async fn answer_clarifications(mut inputs: InputReceiver, clarifications: ClarificationStore) {
    while let Some(input) = inputs.recv().await {
        let request_id = input.request_id.clone();
        if !clarifications.resolve(&request_id, clarification_answers(input)) {
            warn!(
                request = %request_id,
                "The broker answered a question this worker is no longer asking"
            );
        }
    }
}

/// The wire's answers in the clarification store's vocabulary.
pub fn clarification_answers(input: TaskInput) -> Vec<ClarificationAnswer> {
    input
        .answers
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
    usage: Option<TokenUsage>,
    /// What this worker's own delegations spent, summed (AGE-415).
    delegated_usage: Option<TokenUsage>,
    /// This task's tool calls, in the order they started (AGE-467).
    trace: Vec<TraceStep>,
    /// Tool call id → index into `trace`, for a call still waiting on its
    /// result or error.
    open_calls: HashMap<String, usize>,
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
            usage: None,
            delegated_usage: None,
            trace: Vec::new(),
            open_calls: HashMap::new(),
        }
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
            }
            _ => {}
        }
    }

    /// The frame for `event`, or `None` for the events that stay in the child.
    pub fn map(&mut self, event: &SessionEvent) -> Option<ParticipantFrame> {
        self.record_trace_event(event);
        match event {
            SessionEvent::TurnStarted => Some(self.status(TaskState::Working, None)),

            SessionEvent::Text(text) => Some(ParticipantFrame::Artifact {
                task_id: self.task_id.clone(),
                text: text.clone(),
                last_chunk: false,
            }),

            SessionEvent::ApprovalRequested { command, .. } => Some(self.status(
                TaskState::InputRequired,
                Some(format!("approval needed: {command}")),
            )),
            SessionEvent::ClarificationRequested { id, questions } => {
                let asked = questions
                    .first()
                    .map(|q| q.question.clone())
                    .unwrap_or_else(|| "a clarifying question".to_string());
                Some(ParticipantFrame::Status {
                    task_id: self.task_id.clone(),
                    state: TaskState::InputRequired,
                    message: Some(asked),
                    metadata: None,
                    input: Some(InputRequest {
                        id: id.clone(),
                        questions: questions
                            .iter()
                            .map(|q| InputQuestion {
                                id: q.id.clone(),
                                question: q.question.clone(),
                                options: q.options.clone(),
                            })
                            .collect(),
                    }),
                })
            }

            // Usage is held rather than sent: see the module docs.
            SessionEvent::TokenUsage(usage) => {
                self.usage = Some(usage.clone());
                None
            }
            // A grandchild's spend rolls up into this task's number.
            SessionEvent::Delegation(InvokeAgentProgress::Finished {
                usage: Some(usage), ..
            }) => {
                let total = self.delegated_usage.get_or_insert_with(TokenUsage::default);
                add_tokens(total, usage);
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
        ParticipantFrame::Status {
            task_id: self.task_id.clone(),
            state: self.state,
            message: self.failure.clone(),
            metadata: self.terminal_metadata(),
            input: None,
        }
    }

    /// Everything that rides on the terminal status's `metadata`: usage
    /// under `USAGE_METADATA_KEY` (ADR-0011) and, when this task made any
    /// tool calls, the compacted trace under [`TRACE_METADATA_KEY`]
    /// (AGE-467). `None` when neither has anything to report.
    fn terminal_metadata(&self) -> Option<Value> {
        let usage = self.reported_usage();
        let trace = compact_trace(&self.trace);
        if usage.is_none() && trace.is_none() {
            return None;
        }

        let mut metadata = match usage.as_ref().map(usage_metadata) {
            Some(Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        if let Some(trace) = trace {
            metadata.insert(TRACE_METADATA_KEY.to_string(), Value::String(trace));
        }
        Some(Value::Object(metadata))
    }

    /// The task's usage as the parent is told it: this worker's own turn
    /// plus everything it delegated, or `None` when neither reported any.
    fn reported_usage(&self) -> Option<TokenUsage> {
        match (&self.usage, &self.delegated_usage) {
            (None, None) => None,
            (Some(own), None) => Some(own.clone()),
            (None, Some(delegated)) => Some(delegated.clone()),
            (Some(own), Some(delegated)) => {
                let mut total = own.clone();
                add_tokens(&mut total, delegated);
                Some(total)
            }
        }
    }

    fn status(&self, state: TaskState, message: Option<String>) -> ParticipantFrame {
        ParticipantFrame::Status {
            task_id: self.task_id.clone(),
            state,
            message,
            metadata: None,
            input: None,
        }
    }
}

/// Add `usage`'s four token buckets onto `total`.
fn add_tokens(total: &mut TokenUsage, usage: &TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(usage.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(usage.cache_write_tokens);
}

/// The turn's usage, for the terminal status's `metadata`.
fn usage_metadata(usage: &TokenUsage) -> Value {
    json!({
        "usage": {
            "inputTokens": usage.input_tokens,
            "outputTokens": usage.output_tokens,
            "cacheReadTokens": usage.cache_read_tokens,
            "cacheWriteTokens": usage.cache_write_tokens,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::services::{StreamError, StreamErrorKind};

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

    #[test]
    fn usage_rides_in_the_terminal_status_metadata() {
        let mut mapper = TaskMapper::new("task-1");
        let usage = TokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            ..Default::default()
        };
        assert!(
            mapper.map(&SessionEvent::TokenUsage(usage)).is_none(),
            "usage is not a task event"
        );

        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        let metadata = metadata.expect("usage is attached to the terminal status");
        assert_eq!(metadata["usage"]["inputTokens"], 120);
        assert_eq!(metadata["usage"]["outputTokens"], 34);
        // The leader reads it back with core's reader (AGE-415): the two
        // spellings are pinned to each other here.
        let read = chatty_core::services::a2a_client::usage_from_status_metadata(Some(&metadata))
            .expect("the leader can read what the mapper wrote");
        assert_eq!((read.input_tokens, read.output_tokens), (120, 34));
    }

    /// AGE-415: a sub-leader's terminal usage already includes what its own
    /// workers spent, so its parent sees one number for the whole subtree.
    #[test]
    fn a_workers_delegations_roll_up_into_its_terminal_usage() {
        let mut mapper = TaskMapper::new("task-1");
        let worker = |input: u32, output: u32| TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 10,
            cache_write_tokens: 1,
            delegated_to: Some("local-coder".to_string()),
            ..Default::default()
        };
        for usage in [worker(1_000, 100), worker(2_000, 200)] {
            assert!(
                mapper
                    .map(&SessionEvent::Delegation(InvokeAgentProgress::Finished {
                        success: true,
                        result: None,
                        usage: Some(usage),
                    }))
                    .is_none(),
                "a delegation's usage is not a progress line"
            );
        }
        // One delegation reported nothing (a WASM module, say).
        assert!(
            mapper
                .map(&SessionEvent::Delegation(InvokeAgentProgress::Finished {
                    success: true,
                    result: None,
                    usage: None,
                }))
                .is_none()
        );
        mapper.map(&SessionEvent::TokenUsage(TokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            cache_read_tokens: 5,
            cache_write_tokens: 2,
            ..Default::default()
        }));

        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        let metadata = metadata.expect("usage is attached to the terminal status");
        assert_eq!(metadata["usage"]["inputTokens"], 120 + 1_000 + 2_000);
        assert_eq!(metadata["usage"]["outputTokens"], 34 + 100 + 200);
        assert_eq!(metadata["usage"]["cacheReadTokens"], 5 + 10 + 10);
        assert_eq!(metadata["usage"]["cacheWriteTokens"], 2 + 1 + 1);
        assert!(
            metadata["usage"].get("delegatedTo").is_none(),
            "the wire's shape is unchanged: {metadata}"
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
            usage: Some(TokenUsage::new(7, 3)),
        }));
        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        let metadata = metadata.expect("the delegated spend is attached");
        assert_eq!(metadata["usage"]["inputTokens"], 7);
        assert_eq!(metadata["usage"]["outputTokens"], 3);
    }

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
        let ParticipantFrame::Status {
            state,
            message,
            input,
            ..
        } = frame
        else {
            panic!("expected a status frame");
        };
        assert_eq!(state, TaskState::InputRequired);
        assert_eq!(
            message.as_deref(),
            Some("Which database?"),
            "the progress line is the first question"
        );
        let request = input.expect("the request rides with the parked state");
        assert_eq!(
            request.id, "req-1",
            "the store's request id is what the answer resolves"
        );
        assert_eq!(
            request.questions.len(),
            2,
            "every question goes up, not just the first"
        );
        assert_eq!(request.questions[1].options, vec!["eu", "us"]);
        assert_eq!(outcome(&mapper), TaskState::Completed, "parked, not over");
    }

    #[tokio::test]
    async fn answers_from_the_broker_reach_the_waiting_tool() {
        use crate::participant::InputAnswer;
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
        tokio::spawn(answer_clarifications(inputs_rx, store));
        inputs_tx
            .send(TaskInput {
                request_id,
                answers: vec![InputAnswer {
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

    #[test]
    fn a_blocked_tool_parks_the_task_rather_than_ending_it() {
        let mut mapper = TaskMapper::new("task-1");
        let frame = mapper
            .map(&SessionEvent::ApprovalRequested {
                id: "a1".into(),
                command: "rm -rf /".into(),
                is_sandboxed: false,
            })
            .unwrap();
        let (state, message) = status_of(&frame);
        assert_eq!(state, TaskState::InputRequired);
        assert!(message.unwrap().contains("rm -rf /"));
        assert_eq!(
            outcome(&mapper),
            TaskState::Completed,
            "input-required is not terminal; AGE-306 routes it to a human"
        );
    }

    // ── AGE-467: the worker's tool-call trace ───────────────────────────────

    /// The terminal metadata's `trace` string for a mapper, or a panic if the
    /// frame isn't a status.
    fn trace_of(mapper: &TaskMapper) -> Option<String> {
        let ParticipantFrame::Status { metadata, .. } = mapper.terminal() else {
            panic!("expected a status frame");
        };
        metadata.and_then(|m| m["trace"].as_str().map(str::to_string))
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
}
