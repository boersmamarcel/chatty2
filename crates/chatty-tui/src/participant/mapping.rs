//! `SessionEvent` → A2A, the interface AGE-301 exists to shape.
//!
//! [`SessionEvent`] is the output contract of a turn: one event per
//! observable thing the turn did. A2A is a *task* protocol: status updates
//! and artifact updates. ADR-0011's first kill criterion is whether the
//! second can carry the first at the granularity the parent already renders,
//! and this table is the answer being tested.
//!
//! | `SessionEvent` | frame |
//! |---|---|
//! | `TurnStarted` | `status: working` |
//! | `Text` | `artifact` (a chunk, `lastChunk: false`) |
//! | `ToolCallStarted` / `ToolCallResult` / `ToolCallError` | `status: working` with the progress line |
//! | `ToolCallInput` | — |
//! | `ApprovalRequested` / `ClarificationRequested` | `status: input-required` |
//! | `ApprovalResolved` | `status: working` |
//! | `SubAgent` | `status: working` (a grandchild's progress) |
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
//! [`progress_text_for_event`], the same function the `CHATTY_EVENT` path
//! uses.** Rewriting them here would make the two delegation paths differ in
//! the parent's transcript for no reason anyone chose, and the kill criterion
//! is measured by diffing that transcript.
//!
//! **`TurnEnded` produces nothing.** A terminal status ends the A2A task, and
//! one delegated task can span several turns — headless recovery and
//! protocol follow-ups each end a turn. The participant sends exactly one
//! terminal status when the whole delegation is over, built from
//! [`TaskMapper::terminal`].
//!
//! # What A2A cannot carry
//!
//! Token usage. A2A has no notion of it, and inventing a frame would put
//! accounting into the task protocol. It rides in the terminal status's
//! `metadata`, which is where ADR-0011's ledger (AGE-307) reads it.

use chatty_core::models::token_usage::TokenUsage;
use chatty_core::session::SessionEvent;
use chatty_core::tools::progress_text_for_event;
use chatty_protocol_gateway::participant::{ParticipantFrame, TaskState};
use serde_json::{Value, json};
use std::collections::HashMap;

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
            SessionEvent::ClarificationRequested { questions, .. } => {
                let asked = questions
                    .first()
                    .map(|q| q.question.clone())
                    .unwrap_or_else(|| "a clarifying question".to_string());
                Some(self.status(TaskState::InputRequired, Some(asked)))
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
                // Still reported as progress, because the `CHATTY_EVENT` path
                // shows it in the parent's transcript and the A/A diff would
                // otherwise blame this mapping for the difference.
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
