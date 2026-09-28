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
//! # Version 2 (ADR-0020)
//!
//! Every frame, in both directions, carries `"v":2`. A frame without it is
//! refused with an `error` frame naming v2 and the connection is closed;
//! there is no v1 fallback. [`encode_frame`] and [`decode_frame`] are the
//! only way frames go on or come off the wire, so the check cannot be
//! skipped by one side.
//!
//! A connection is made by the broker, not by the worker: the broker admits
//! a node, creates a socket pair, keeps one end and hands the other to the
//! child it spawns. The connection *is* the identity, so the worker's
//! `hello` names nothing — a card's `name` is ignored — and the broker's
//! `welcome` tells the worker who it is.
//!
//! # A session
//!
//! ```text
//! participant → {"v":2,"type":"hello","card":{"name":"",…}}
//! broker      → {"v":2,"type":"welcome","name":"local-coder-0","scope":"root","owner":null}
//! broker      → {"v":2,"type":"task","taskId":"task-…","text":"summarise foo.rs"}
//! participant → {"v":2,"type":"status","taskId":"task-…","state":"working","message":"read_file"}
//! participant → {"v":2,"type":"artifact","taskId":"task-…","text":"foo.rs defines…","lastChunk":false}
//! participant → {"v":2,"type":"status","taskId":"task-…","state":"completed"}
//! ```
//!
//! # Calls (BI-4, AGE-636)
//!
//! A worker reaches other agents over the same connection: its
//! `invoke_agent` and `list_agents` send a `call`, and the broker runs it
//! as the node this connection names — the call says nothing about who is
//! calling. Several calls can be in flight within one task; every reply
//! carries the call's `id`, and the replies of different calls interleave
//! in whatever order they finish.
//!
//! ```text
//! participant → {"v":2,"type":"call","id":1,"method":"invoke_agent",
//!                "params":{"agent":"local-reviewer","prompt":"review it","handle":null,"include_trace":false}}
//! participant → {"v":2,"type":"call","id":2,"method":"list_agents"}
//! broker      → {"v":2,"type":"call_result","id":2,"result":[{"name":"local-reviewer","origin":"local",…}]}
//! broker      → {"v":2,"type":"call_progress","id":1,"event":{"Step":"read_file"}}
//! broker      → {"v":2,"type":"call_progress","id":1,"event":{"Text":"Looks good."}}
//! broker      → {"v":2,"type":"call_result","id":1,
//!                "result":{"success":true,"response":"Looks good.","metadata":{"usage":[…]}}}
//! ```
//!
//! `event` is an `InvokeAgentProgress` as JSON (`Step` for a line about the
//! callee's work, `Text` for its answer as it streams). A call that cannot
//! run at all ends with `call_error` and `error: {kind, message}`; a callee
//! whose task failed ends with a `call_result` whose `success` is false,
//! exactly as a failed A2A task. When the worker's connection closes, the
//! broker cancels every call still in flight on it, which reaps the
//! workers those calls started.
//!
//! # A question on a call (BI-5, AGE-637)
//!
//! A callee that asks a question parks its task (below); the broker tells
//! the calling worker with `call_input_required`, naming the call and the
//! parked task, and the worker's answer goes back up as `call_input` with
//! the same `input` shape an `input` frame carries. The broker delivers it
//! only to a task parked on that call, so a worker can answer its own
//! callees and nobody else's. This is what lets a grandchild's `ask_user`
//! climb to the root's human and its answer come back down, however many
//! workers sit in between: each hop re-asks the question on its own
//! clarification store, which parks its own task toward its caller.
//!
//! ```text
//! broker      → {"v":2,"type":"call_input_required","id":1,"task":"task-…",
//!                "request":{"id":"req-…","questions":[{"id":"q1","question":"Which database?","options":[]}]}}
//! participant → {"v":2,"type":"call_input","id":1,"task":"task-…",
//!                "input":{"requestId":"req-…","answers":[{"id":"q1","answer":"SQLite","custom":false}]}}
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
//! participant → {"v":2,"type":"status","taskId":"task-…","state":"input-required",
//!                "message":"Which database?",
//!                "input":{"id":"req-…","questions":[{"id":"q1","question":"Which database?","options":["Postgres","SQLite"]}]}}
//! broker      → {"v":2,"type":"input","taskId":"task-…",
//!                "input":{"requestId":"req-…","answers":[{"id":"q1","answer":"Postgres","custom":false}]}}
//! participant → {"v":2,"type":"status","taskId":"task-…","state":"working","message":"✓ ask_user"}
//! ```

use chatty_fabric::{CallError, CallRequest, ConversationScope, NodeName, SpawnContext};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The participant protocol's version. Every frame carries it as `v`.
pub const PROTOCOL_VERSION: u64 = 2;

/// Why a line off the socket is not a frame this build accepts.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// A frame without `v`: a v1 peer, or no peer of ours at all.
    #[error("frame has no \"v\"; the participant protocol is v2 only")]
    MissingVersion,
    /// A frame for a version this build does not speak.
    #[error("frame is version {0}; the participant protocol is v2 only")]
    WrongVersion(Value),
    /// Not JSON, not an object, or not a frame of this direction.
    #[error("malformed frame: {0}")]
    Malformed(String),
}

impl FrameError {
    /// Whether the peer speaks another protocol version, which ends the
    /// connection, rather than having sent one bad frame.
    pub fn is_version(&self) -> bool {
        matches!(self, Self::MissingVersion | Self::WrongVersion(_))
    }
}

/// One frame as a line, without the trailing newline, carrying `"v":2`.
pub fn encode_frame<F: Serialize>(frame: &F) -> Result<String, serde_json::Error> {
    let Value::Object(mut fields) = serde_json::to_value(frame)? else {
        return Err(serde::ser::Error::custom(
            "a frame serializes to a JSON object",
        ));
    };
    fields.insert("v".to_string(), Value::from(PROTOCOL_VERSION));
    serde_json::to_string(&Value::Object(fields))
}

/// One line off the socket as a frame, refusing anything that is not v2.
pub fn decode_frame<F: DeserializeOwned>(line: &str) -> Result<F, FrameError> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| FrameError::Malformed(e.to_string()))?;
    let Value::Object(mut fields) = value else {
        return Err(FrameError::Malformed(
            "a frame is a JSON object".to_string(),
        ));
    };
    match fields.remove("v") {
        None => return Err(FrameError::MissingVersion),
        Some(v) if v.as_u64() == Some(PROTOCOL_VERSION) => {}
        Some(other) => return Err(FrameError::WrongVersion(other)),
    }
    serde_json::from_value(Value::Object(fields)).map_err(|e| FrameError::Malformed(e.to_string()))
}

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
    /// The caller token of the broker worker that asked for this task, when
    /// one did ([`CALLER_HEADER`]). Stays with the broker: a runner reads it
    /// to tell a worker delegating on its own model endpoint from any other
    /// caller (AGE-628), and it is not part of the task frame.
    pub caller: Option<String>,
    /// Whether the worker should capture its conversation at this task's
    /// terminal status (RC-0, AGE-649). Opt-in and off by default, so an
    /// ordinary task's frames are unchanged.
    pub capture_conversation: bool,
    /// Where the worker is spawned and what it may reach (BI-5), already
    /// clamped by the broker. `None` leaves a runner to its own defaults:
    /// the root's workspace and settings.
    pub spawn_context: Option<SpawnContext>,
}

/// The header a broker worker's `invoke_agent` puts its caller token in, on
/// a call back into a broker. chatty-core's `invoke_agent_tool` spells it
/// too; a test there keeps the two the same.
pub const CALLER_HEADER: &str = "x-chatty-broker-caller";

/// The environment variable a runner hands each worker its caller token in.
pub const CALLER_ENV: &str = "CHATTY_BROKER_CALLER";

impl DelegatedTask {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bearer: None,
            caller: None,
            capture_conversation: false,
            spawn_context: None,
        }
    }

    /// The context the worker is spawned with (BI-5).
    pub fn with_spawn_context(mut self, context: Option<SpawnContext>) -> Self {
        self.spawn_context = context;
        self
    }

    pub fn with_caller(mut self, caller: Option<String>) -> Self {
        self.caller = caller;
        self
    }

    pub fn with_bearer(mut self, bearer: Option<TaskBearer>) -> Self {
        self.bearer = bearer;
        self
    }

    /// Ask the worker to capture its conversation at this task's terminal
    /// status (RC-0, AGE-649).
    pub fn with_capture_conversation(mut self, capture: bool) -> Self {
        self.capture_conversation = capture;
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

/// What a participant publishes about itself in its `hello`.
///
/// `name` is the address callers reach it at (`/a2a/{name}`), and it is the
/// broker's to give: whatever a worker puts here is replaced by the name
/// the broker admitted its connection under (ADR-0020). A worker leaves it
/// empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantCard {
    #[serde(default)]
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
    /// The card's `name` is ignored: the connection already names the node.
    Hello {
        #[serde(default)]
        card: ParticipantCard,
    },
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
    /// A call to another agent, made as the node this connection names
    /// (BI-4). `id` is the worker's own, unique on this connection; the
    /// replies carry it back. The request is flattened in, so the wire
    /// reads `{"type":"call","id":1,"method":…,"params":…}`.
    #[serde(rename = "call")]
    Call {
        id: u64,
        #[serde(flatten)]
        request: CallRequest,
    },
    /// The answer to a question a callee of call `id` asked
    /// ([`BrokerFrame::CallInputRequired`]): `task` is the callee's parked
    /// task, `input` the same shape an [`BrokerFrame::Input`] carries
    /// (BI-5).
    #[serde(rename = "call_input")]
    CallInput {
        id: u64,
        task: String,
        input: TaskInput,
    },
}

/// A frame from the broker to a participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrokerFrame {
    /// The answer to `hello`: who this connection is. `name` is what
    /// callers address, `scope` the conversation the node works for, and
    /// `owner` the node that asked for it (`None` when the root did).
    Welcome {
        name: NodeName,
        scope: ConversationScope,
        owner: Option<NodeName>,
    },
    /// The connection is refused: a frame that is not v2, a first frame
    /// that is not `hello`, or any registration on the shared socket. The
    /// broker closes the connection after this frame.
    Error { reason: String },
    /// Work. Answer with `Status` / `Artifact` frames carrying this `taskId`
    /// and end with a terminal state. `bearer` is the caller's token when
    /// they presented one (AGE-371); absent on the wire otherwise, so a
    /// broker and a worker from either side of that change still agree.
    /// `captureConversation` asks the worker to attach its conversation to
    /// the terminal status (RC-0, AGE-649); also absent on the wire when
    /// `false`, for the same reason.
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer: Option<TaskBearer>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        capture_conversation: bool,
        /// The context this worker was spawned with (BI-5): its workspace
        /// root, base branch, roster, verification command and endpoint.
        /// Absent on the wire for a task given without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_context: Option<SpawnContext>,
    },
    /// The caller went away. Stop working on `taskId`; no reply is required.
    #[serde(rename_all = "camelCase")]
    Cancel { task_id: String },
    /// The answer to a task parked in `input-required`. Resolve the request
    /// it names and carry on; the next `Status` frame un-parks the task.
    #[serde(rename_all = "camelCase")]
    Input { task_id: String, input: TaskInput },
    /// Progress on call `id`: an `InvokeAgentProgress`, as JSON.
    #[serde(rename = "call_progress")]
    CallProgress { id: u64, event: Value },
    /// Call `id` is over, and this is what it returned.
    #[serde(rename = "call_result")]
    CallResult { id: u64, result: Value },
    /// Call `id` could not be carried out, and is over.
    #[serde(rename = "call_error")]
    CallError { id: u64, error: CallError },
    /// A callee of call `id` parked its task `task` on a question
    /// (`request`, an [`InputRequest`] as JSON). The call stays open; answer
    /// with [`ParticipantFrame::CallInput`] (BI-5).
    #[serde(rename = "call_input_required")]
    CallInputRequired {
        id: u64,
        task: String,
        request: Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AGE-628: the worker's `invoke_agent` (chatty-core) and the broker
    /// spell the caller token's header and variable the same way.
    #[test]
    fn the_caller_token_is_spelled_alike_on_both_sides() {
        use chatty_core::tools::invoke_agent_tool::{BROKER_CALLER_ENV, BROKER_CALLER_HEADER};
        assert_eq!(CALLER_ENV, BROKER_CALLER_ENV);
        assert_eq!(CALLER_HEADER, BROKER_CALLER_HEADER);
    }

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
            capture_conversation: false,
            spawn_context: None,
        })
        .unwrap();
        assert_eq!(json["type"], "task");
        assert_eq!(json["taskId"], "task-1");
        assert_eq!(json["text"], "do it");
        assert!(
            json.get("bearer").is_none(),
            "a task without a bearer is the frame it was before AGE-371"
        );
        assert!(
            json.get("captureConversation").is_none(),
            "a task that does not ask for capture is the frame it was before AGE-649"
        );
    }

    #[test]
    fn a_task_frame_carries_the_bearer_and_reads_one_without() {
        let json = serde_json::to_value(BrokerFrame::Task {
            task_id: "task-1".into(),
            text: "do it".into(),
            bearer: Some(TaskBearer::new("eyJ.token")),
            capture_conversation: false,
            spawn_context: None,
        })
        .unwrap();
        assert_eq!(json["bearer"], "eyJ.token");

        let old: BrokerFrame =
            serde_json::from_str(r#"{"type":"task","taskId":"t","text":"x"}"#).unwrap();
        let BrokerFrame::Task {
            bearer,
            capture_conversation,
            ..
        } = old
        else {
            panic!("expected a task frame");
        };
        assert!(bearer.is_none());
        assert!(
            !capture_conversation,
            "an old frame without the field means off"
        );
    }

    #[test]
    fn a_task_frame_carries_capture_conversation_and_reads_one_without() {
        let json = serde_json::to_value(BrokerFrame::Task {
            task_id: "task-1".into(),
            text: "do it".into(),
            bearer: None,
            capture_conversation: true,
            spawn_context: None,
        })
        .unwrap();
        assert_eq!(json["captureConversation"], true);

        let back: BrokerFrame = serde_json::from_value(json).unwrap();
        let BrokerFrame::Task {
            capture_conversation,
            ..
        } = back
        else {
            panic!("expected a task frame");
        };
        assert!(capture_conversation);
    }

    #[test]
    fn the_bearer_does_not_debug_print() {
        let frame = BrokerFrame::Task {
            task_id: "t".into(),
            text: "x".into(),
            bearer: Some(TaskBearer::new("secret-token")),
            capture_conversation: false,
            spawn_context: None,
        };
        let printed = format!("{frame:?}");
        assert!(!printed.contains("secret-token"), "{printed}");
        assert!(printed.contains("[redacted]"));
    }

    #[test]
    fn hello_frame_round_trips_a_card() {
        let line = r#"{"v":2,"type":"hello","card":{"name":"worker-1","description":"a worker",
                       "skills":[{"name":"edit"}]}}"#;
        let frame: ParticipantFrame = decode_frame(line).unwrap();
        let ParticipantFrame::Hello { card } = frame else {
            panic!("expected a hello frame");
        };
        assert_eq!(card.name, "worker-1", "carried, and ignored by the broker");
        assert_eq!(card.skills[0].name, "edit");
        // Absent optional fields default rather than failing the connection.
        assert_eq!(card.version, "");
        assert!(card.display_name.is_none());

        let bare: ParticipantFrame = decode_frame(r#"{"v":2,"type":"hello"}"#).unwrap();
        assert!(matches!(bare, ParticipantFrame::Hello { card } if card.name.is_empty()));
    }

    #[test]
    fn every_encoded_frame_carries_v2() {
        let line = encode_frame(&BrokerFrame::Cancel {
            task_id: "t".into(),
        })
        .unwrap();
        let json: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json["v"], 2, "{line}");
        let back: BrokerFrame = decode_frame(&line).unwrap();
        assert!(matches!(back, BrokerFrame::Cancel { task_id } if task_id == "t"));
    }

    #[test]
    fn a_frame_without_v_or_with_another_is_refused() {
        let v1 = r#"{"type":"register","card":{"name":"worker-1"}}"#;
        let err = decode_frame::<ParticipantFrame>(v1).unwrap_err();
        assert!(matches!(err, FrameError::MissingVersion));
        assert!(err.is_version());
        assert!(err.to_string().contains("v2"), "{err}");

        let v3 = r#"{"v":3,"type":"hello"}"#;
        let err = decode_frame::<ParticipantFrame>(v3).unwrap_err();
        assert!(matches!(err, FrameError::WrongVersion(_)));
        assert!(err.to_string().contains("v2"), "{err}");

        let err = decode_frame::<ParticipantFrame>(r#"{"v":2,"type":"register"}"#).unwrap_err();
        assert!(
            !err.is_version(),
            "a v2 frame of an unknown type is malformed, not another version"
        );
    }

    #[test]
    fn welcome_names_the_node_its_scope_and_its_owner() {
        let frame: BrokerFrame = decode_frame(
            r#"{"v":2,"type":"welcome","name":"local-coder-0","scope":"root","owner":null}"#,
        )
        .unwrap();
        let BrokerFrame::Welcome { name, scope, owner } = frame else {
            panic!("expected a welcome frame");
        };
        assert_eq!(name.as_str(), "local-coder-0");
        assert_eq!(scope.as_str(), "root");
        assert!(owner.is_none());
    }

    #[test]
    fn call_frames_use_the_documented_wire_names() {
        use chatty_fabric::InvokeAgentParams;

        let line = encode_frame(&ParticipantFrame::Call {
            id: 7,
            request: CallRequest::InvokeAgent(InvokeAgentParams {
                agent: "local-reviewer".into(),
                prompt: "review it".into(),
                handle: None,
                include_trace: false,
                spawn_context: None,
            }),
        })
        .unwrap();
        let json: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json["type"], "call");
        assert_eq!(json["id"], 7);
        assert_eq!(json["method"], "invoke_agent");
        assert_eq!(json["params"]["agent"], "local-reviewer");
        let back: ParticipantFrame = decode_frame(&line).unwrap();
        assert!(matches!(
            back,
            ParticipantFrame::Call { id: 7, request: CallRequest::InvokeAgent(p) } if p.prompt == "review it"
        ));

        let list: ParticipantFrame =
            decode_frame(r#"{"v":2,"type":"call","id":2,"method":"list_agents"}"#).unwrap();
        assert!(matches!(
            list,
            ParticipantFrame::Call {
                id: 2,
                request: CallRequest::ListAgents
            }
        ));

        for (frame, kind) in [
            (
                BrokerFrame::CallProgress {
                    id: 1,
                    event: serde_json::json!({"Step": "read_file"}),
                },
                "call_progress",
            ),
            (
                BrokerFrame::CallResult {
                    id: 1,
                    result: serde_json::json!({"success": true}),
                },
                "call_result",
            ),
            (
                BrokerFrame::CallError {
                    id: 1,
                    error: CallError::UnknownAgent("nobody".into()),
                },
                "call_error",
            ),
        ] {
            let json: Value = serde_json::from_str(&encode_frame(&frame).unwrap()).unwrap();
            assert_eq!(json["type"], kind);
            assert_eq!(json["id"], 1);
        }
        let error: BrokerFrame = decode_frame(
            r#"{"v":2,"type":"call_error","id":3,"error":{"kind":"unknown_agent","message":"x"}}"#,
        )
        .unwrap();
        assert!(matches!(
            error,
            BrokerFrame::CallError { id: 3, error: CallError::UnknownAgent(m) } if m == "x"
        ));
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
