//! The frames a local participant and the broker exchange over the socket.
//!
//! One JSON object per line, in both directions. Newline-delimited JSON is
//! enough because the socket carries one participant and its tasks are
//! multiplexed by `taskId`, so there is no framing problem a length prefix
//! would solve — and a line is greppable in a log, which a length prefix is
//! not.
//!
//! The frames are deliberately *not* A2A: A2A is the broker's public wire
//! format, and a child process is not a public endpoint. The broker maps
//! between the two (see [`super::registry`]), which is the seam that lets a
//! participant stream partial progress without minting JSON-RPC envelopes.
//!
//! # A session
//!
//! ```text
//! participant → {"type":"register","card":{"name":"worker-1",…}}
//! broker      → {"type":"registered","name":"worker-1"}
//! broker      → {"type":"task","taskId":"task-…","text":"summarise foo.rs"}
//! participant → {"type":"status","taskId":"task-…","state":"working","message":"read_file"}
//! participant → {"type":"artifact","taskId":"task-…","text":"foo.rs defines…","lastChunk":false}
//! participant → {"type":"status","taskId":"task-…","state":"completed"}
//! ```
//!
//! # A parked task
//!
//! A worker that asks a question (`ask_user`) parks its task in
//! `input-required` and says what it is waiting for; the answer comes back
//! down as an `input` frame on the same task, and the task resumes
//! (ADR-0011 C7, AGE-306).
//!
//! ```text
//! participant → {"type":"status","taskId":"task-…","state":"input-required",
//!                "message":"Which database?",
//!                "input":{"id":"req-…","questions":[{"id":"q1","question":"Which database?","options":["Postgres","SQLite"]}]}}
//! broker      → {"type":"input","taskId":"task-…",
//!                "input":{"requestId":"req-…","answers":[{"id":"q1","answer":"Postgres","custom":false}]}}
//! participant → {"type":"status","taskId":"task-…","state":"working","message":"✓ ask_user"}
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The state of one task, in A2A's vocabulary.
///
/// A2A's own spelling is kebab-case (`input-required`), and these values are
/// copied verbatim into the `status.state` field the broker serves, so the
/// serde renaming here is part of the public contract rather than a style
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    /// The participant is blocked on a human. Routing this up the chain to
    /// `ask_user` is AGE-306; the broker forwards the state either way.
    InputRequired,
    Completed,
    Failed,
    Canceled,
}

impl TaskState {
    /// Whether this state ends the task. A terminal status is the last
    /// update a caller sees, and the broker drops the task when it arrives.
    ///
    /// `InputRequired` is *not* terminal: the task is parked, not over.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Via serde so the wire spelling can only be defined once.
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        f.write_str(&s)
    }
}

/// One question a parked task is waiting on.
///
/// Field for field the shape of chatty-core's `ClarifyingQuestion`, so the
/// two serialize identically; it is spelled out here because the wire's
/// schema belongs with the wire, and this crate does not depend on
/// chatty-core without the `worker` feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputQuestion {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// What a task in `input-required` is waiting for: one `ask_user` call.
///
/// `id` is the worker's own request id — the key its clarification store
/// resolves on — and it rides up and back down unchanged so the answer
/// lands on the call that asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRequest {
    pub id: String,
    pub questions: Vec<InputQuestion>,
}

/// The answer to one [`InputQuestion`]; the shape of chatty-core's
/// `ClarificationAnswer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAnswer {
    pub id: String,
    pub answer: String,
    #[serde(default)]
    pub custom: bool,
}

/// The caller's bearer token, carried to the worker that runs their task
/// (AGE-371).
///
/// A hosted worker validates it exactly as an HTTP request's bearer is
/// validated and runs the task as that user; a local worker has no use for
/// it and ignores it. It rides the task frame, never the worker's
/// environment: a microVM may serve successive users, and the identity
/// belongs to the turn, not the machine.
///
/// `Debug` prints nothing of it, and `serde` sees straight through to the
/// string — the socket between broker and worker is the one place it is
/// meant to be in the clear.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskBearer(String);

impl TaskBearer {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token itself. Named so the read is visible at the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TaskBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskBearer([redacted])")
    }
}

/// Work for a worker: the prompt, and whose task it is.
///
/// What [`BrokerFrame::Task`] carries past its id, in one value so the
/// broker's submit path, a virtual agent's `run_task` and the worker's turn
/// all take the same thing and a bearer cannot be dropped between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedTask {
    pub text: String,
    /// `None` for a caller that presented no bearer — a desktop parent
    /// delegating to a local worker. A hosted worker refuses such a task.
    pub bearer: Option<TaskBearer>,
}

impl DelegatedTask {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bearer: None,
        }
    }

    pub fn with_bearer(mut self, bearer: Option<TaskBearer>) -> Self {
        self.bearer = bearer;
        self
    }
}

/// The answers for a parked task: A2A `message/send` on the same task id,
/// in the broker's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    /// The [`InputRequest::id`] this answers.
    pub request_id: String,
    pub answers: Vec<InputAnswer>,
}

/// One skill on a participant's agent card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// What a participant publishes about itself at registration.
///
/// `name` is the address: callers reach this participant at `/a2a/{name}`,
/// so it has to be unique across the broker and stable for the connection's
/// lifetime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantCard {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub skills: Vec<ParticipantSkill>,
}

/// A frame from a participant to the broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ParticipantFrame {
    /// The first frame on a connection. A second one is a protocol error.
    Register { card: ParticipantCard },
    /// A task moved, optionally with progress text. The broker turns this
    /// into an A2A `TaskStatusUpdateEvent`.
    ///
    /// `metadata` is copied verbatim into the A2A status's `metadata` field.
    /// It is where a turn's token usage rides back: A2A has no usage concept
    /// — usage belongs to the ledger, not to the task protocol — and
    /// inventing a frame for it would put accounting in the wire format.
    /// The broker's ledger (AGE-307) reads it from there.
    ///
    /// `input` accompanies `input-required` and says what the task is
    /// waiting for. The broker serves it to the caller under the A2A
    /// status's `metadata.clarification`, and the caller's answer comes
    /// back as [`BrokerFrame::Input`].
    #[serde(rename_all = "camelCase")]
    Status {
        task_id: String,
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<InputRequest>,
    },
    /// A chunk of the task's output, in stream order. The broker turns this
    /// into an A2A `TaskArtifactUpdateEvent`.
    #[serde(rename_all = "camelCase")]
    Artifact {
        task_id: String,
        text: String,
        #[serde(default)]
        last_chunk: bool,
    },
}

/// A frame from the broker to a participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrokerFrame {
    /// Registration accepted. `name` is what callers address; it is echoed
    /// so a participant that let the broker pick one still learns it.
    Registered { name: String },
    /// Registration refused — a duplicate name, or a card without one. The
    /// broker closes the connection after this frame.
    Rejected { reason: String },
    /// Work. Answer with `Status` / `Artifact` frames carrying this `taskId`
    /// and end with a terminal state. `bearer` is the caller's token when
    /// they presented one (AGE-371); absent on the wire otherwise, so a
    /// broker and a worker from either side of that change still agree.
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer: Option<TaskBearer>,
    },
    /// The caller went away. Stop working on `taskId`; no reply is required.
    #[serde(rename_all = "camelCase")]
    Cancel { task_id: String },
    /// The answer to a task parked in `input-required`. Resolve the request
    /// it names and carry on; the next `Status` frame un-parks the task.
    #[serde(rename_all = "camelCase")]
    Input { task_id: String, input: TaskInput },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_frames_use_the_documented_wire_names() {
        let frame = ParticipantFrame::Status {
            task_id: "task-1".into(),
            state: TaskState::InputRequired,
            message: Some("which file?".into()),
            metadata: None,
            input: None,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["taskId"], "task-1");
        assert_eq!(json["state"], "input-required");
        assert_eq!(json["message"], "which file?");
        assert!(
            json.get("metadata").is_none(),
            "an absent metadata field stays off the wire"
        );
        assert!(
            json.get("input").is_none(),
            "an absent input field stays off the wire"
        );
    }

    #[test]
    fn a_parked_task_says_what_it_is_waiting_for() {
        let frame = ParticipantFrame::Status {
            task_id: "task-1".into(),
            state: TaskState::InputRequired,
            message: Some("Which database?".into()),
            metadata: None,
            input: Some(InputRequest {
                id: "req-1".into(),
                questions: vec![InputQuestion {
                    id: "q1".into(),
                    question: "Which database?".into(),
                    options: vec!["Postgres".into(), "SQLite".into()],
                }],
            }),
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["input"]["id"], "req-1");
        assert_eq!(json["input"]["questions"][0]["id"], "q1");
        assert_eq!(json["input"]["questions"][0]["options"][1], "SQLite");

        let back: ParticipantFrame = serde_json::from_value(json).unwrap();
        let ParticipantFrame::Status { input, .. } = back else {
            panic!("expected a status frame");
        };
        assert_eq!(input.unwrap().questions.len(), 1);
    }

    #[test]
    fn an_input_frame_carries_the_answers_under_the_request_id() {
        let json = serde_json::to_value(BrokerFrame::Input {
            task_id: "task-1".into(),
            input: TaskInput {
                request_id: "req-1".into(),
                answers: vec![InputAnswer {
                    id: "q1".into(),
                    answer: "Postgres".into(),
                    custom: false,
                }],
            },
        })
        .unwrap();
        assert_eq!(json["type"], "input");
        assert_eq!(json["taskId"], "task-1");
        assert_eq!(json["input"]["requestId"], "req-1");
        assert_eq!(json["input"]["answers"][0]["answer"], "Postgres");

        // `custom` is optional on the way in: a caller that only ever picks
        // an option need not say so.
        let line = r#"{"type":"input","taskId":"t","input":{"requestId":"r","answers":[{"id":"q1","answer":"x"}]}}"#;
        let frame: BrokerFrame = serde_json::from_str(line).unwrap();
        let BrokerFrame::Input { input, .. } = frame else {
            panic!("expected an input frame");
        };
        assert!(!input.answers[0].custom);
    }

    #[test]
    fn status_metadata_round_trips() {
        let line = r#"{"type":"status","taskId":"t","state":"completed",
                       "metadata":{"usage":{"inputTokens":12}}}"#;
        let frame: ParticipantFrame = serde_json::from_str(line).unwrap();
        let ParticipantFrame::Status { metadata, .. } = frame else {
            panic!("expected a status frame");
        };
        assert_eq!(metadata.unwrap()["usage"]["inputTokens"], 12);
    }

    #[test]
    fn broker_frames_use_the_documented_wire_names() {
        let json = serde_json::to_value(BrokerFrame::Task {
            task_id: "task-1".into(),
            text: "do it".into(),
            bearer: None,
        })
        .unwrap();
        assert_eq!(json["type"], "task");
        assert_eq!(json["taskId"], "task-1");
        assert_eq!(json["text"], "do it");
        assert!(
            json.get("bearer").is_none(),
            "a task without a bearer is the frame it was before AGE-371"
        );
    }

    #[test]
    fn a_task_frame_carries_the_bearer_and_reads_one_without() {
        let json = serde_json::to_value(BrokerFrame::Task {
            task_id: "task-1".into(),
            text: "do it".into(),
            bearer: Some(TaskBearer::new("eyJ.token")),
        })
        .unwrap();
        assert_eq!(json["bearer"], "eyJ.token");

        let old: BrokerFrame =
            serde_json::from_str(r#"{"type":"task","taskId":"t","text":"x"}"#).unwrap();
        let BrokerFrame::Task { bearer, .. } = old else {
            panic!("expected a task frame");
        };
        assert!(bearer.is_none());
    }

    #[test]
    fn the_bearer_does_not_debug_print() {
        let frame = BrokerFrame::Task {
            task_id: "t".into(),
            text: "x".into(),
            bearer: Some(TaskBearer::new("secret-token")),
        };
        let printed = format!("{frame:?}");
        assert!(!printed.contains("secret-token"), "{printed}");
        assert!(printed.contains("[redacted]"));
    }

    #[test]
    fn register_frame_round_trips_a_card() {
        let line = r#"{"type":"register","card":{"name":"worker-1","description":"a worker",
                       "skills":[{"name":"edit"}]}}"#;
        let frame: ParticipantFrame = serde_json::from_str(line).unwrap();
        let ParticipantFrame::Register { card } = frame else {
            panic!("expected a register frame");
        };
        assert_eq!(card.name, "worker-1");
        assert_eq!(card.skills[0].name, "edit");
        // Absent optional fields default rather than failing the connection.
        assert_eq!(card.version, "");
        assert!(card.display_name.is_none());
    }

    #[test]
    fn artifact_last_chunk_defaults_to_false() {
        let frame: ParticipantFrame =
            serde_json::from_str(r#"{"type":"artifact","taskId":"t","text":"x"}"#).unwrap();
        let ParticipantFrame::Artifact { last_chunk, .. } = frame else {
            panic!("expected an artifact frame");
        };
        assert!(!last_chunk, "a chunk is only the last one if it says so");
    }

    #[test]
    fn only_completed_failed_and_canceled_end_a_task() {
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::Submitted.is_terminal());
        assert!(
            !TaskState::InputRequired.is_terminal(),
            "a task waiting on a human is parked, not over (AGE-306)"
        );
    }

    #[test]
    fn task_state_displays_its_wire_spelling() {
        assert_eq!(TaskState::InputRequired.to_string(), "input-required");
        assert_eq!(TaskState::Working.to_string(), "working");
    }
}
