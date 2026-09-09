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
//! `metadata`, which is where ADR-0011's ledger (AGE-307) reads it.

use crate::participant::{InputQuestion, InputRequest, ParticipantFrame, TaskInput, TaskState};
use chatty_core::models::clarification_store::{ClarificationAnswer, ClarificationStore};
use chatty_core::models::token_usage::TokenUsage;
use chatty_core::session::SessionEvent;
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

/// Folds one delegated task's events into frames.
pub struct TaskMapper {
    task_id: String,
    /// Tool call id → name, until its result arrives.
    tool_names: HashMap<String, String>,
    state: TaskState,
    failure: Option<String>,
    usage: Option<TokenUsage>,
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
        }
    }

    /// The frame for `event`, or `None` for the events that stay in the child.
    pub fn map(&mut self, event: &SessionEvent) -> Option<ParticipantFrame> {
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
            metadata: self.usage.as_ref().map(usage_metadata),
            input: None,
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
}
