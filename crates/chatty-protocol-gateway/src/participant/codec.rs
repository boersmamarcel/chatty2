//! The v3 envelope, and the per-connection codec that puts every frame in it
//! (ADR-0021 § 1, EN-1).
//!
//! Every line on a worker's socket, in both directions, is one of four
//! shapes:
//!
//! ```text
//! {"v":3,"id":7,"method":"agent.invoke","params":{…}}   request
//! {"v":3,"id":7,"result":{…}}                           result
//! {"v":3,"id":7,"error":{"kind":"refused","message":"…"}}  error
//! {"v":3,"method":"task.event","params":{…}}            notification
//! ```
//!
//! A line is decoded in two steps: the envelope first, with `params`,
//! `result` and `error` left as raw JSON, then the typed method — and a
//! result or an error only once its id has been matched to a request this
//! side made. Exactly one of `method`, `result` or `error` is present; `id`
//! is absent only on a notification; a `v` other than 3 is a decode error.
//!
//! # Direction is a type
//!
//! What a worker may send ([`WorkerRequest`], [`WorkerNotification`]) and
//! what the broker may send ([`BrokerRequest`], [`BrokerNotification`]) are
//! separate enums. A method the peer may not send does not decode, and a
//! decode error closes the connection.
//!
//! | Sender | Requests | Notifications |
//! |---|---|---|
//! | worker | `session.hello`, `agent.invoke`, `agent.list`, `mailbox.post`, `human.approve` | `task.event`, `req.cancel`, interim `task.input_required`, `call.input` |
//! | broker | `task.run` | `req.progress`, `req.cancel`, interim `task.input`, `call.input_required`, `call.input_withdrawn` |
//!
//! `human.approve` (EN-2a) is a worker's request only: the broker answers
//! it with the root's verdict and never sends one, so a worker that is sent
//! one closes the connection. The interim notifications carry ADR-0021's
//! *step 2* question rows unchanged in meaning until EN-2b replaces them
//! with `human.ask`, which is also when that request joins the table.
//!
//! # Ids
//!
//! Each side numbers its own requests from 1, per connection, and never
//! reuses one. Reusing an id that is still in flight closes the connection.
//! Results, errors, `req.progress` and `task.event` name the *receiver's*
//! request (a `task.event` names its task's `task.run`); `req.cancel` names
//! the *sender's*. A response, progress, event or cancel naming no request
//! in flight — unknown, duplicate or late — is dropped and logged
//! (rate-limited), never fatal.
//!
//! The codec is what keeps that state, so the rest of the broker and the
//! worker still speak in [`ParticipantFrame`]s and [`BrokerFrame`]s: a
//! task's A2A `taskId` rides in `task.run`'s params and is mapped to and
//! from that request's id here, and a worker's call ids are its transport's
//! own, mapped to request ids here. The broker's calls are keyed by the
//! request id the worker gave them.
//!
//! One codec serves one connection; its clones share state, so the read
//! half and the write half of a connection each hold one.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use chatty_fabric::{
    ApprovalRequest, ApprovalVerdict, CallError, CallRequest, ConversationScope, HandoffContract,
    InvokeAgentParams, NodeName, Remaining, SendMessageParams, SpawnContext, SwarmItem,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use tracing::warn;

use super::protocol::{
    BrokerFrame, InputRequest, ParticipantCard, ParticipantFrame, TaskBearer, TaskInput, TaskState,
};

/// The participant protocol's version. Every line carries it as `v`.
pub const PROTOCOL_VERSION: u64 = 3;

/// Why a line off the socket is not one this connection accepts. Every one
/// closes the connection.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Not JSON, not an envelope, or not a method's params.
    #[error("malformed frame: {0}")]
    Malformed(String),
    /// An envelope for a version this build does not speak.
    #[error("frame is version {0}; the participant protocol is v3 only")]
    WrongVersion(u64),
    /// A method this peer may not send (or no method at all).
    #[error("'{0}' is not a method this peer may send")]
    WrongDirection(String),
    /// A request id the peer already has in flight.
    #[error("request id {0} is already in flight")]
    ReusedId(u64),
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

/// One line, as read: the envelope with its payload still raw.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    v: u64,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default, borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(default, deserialize_with = "present")]
    params: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    error: Option<Box<RawValue>>,
}

/// A present field is `Some`, even when it is `null`: a `null` result is
/// still a result.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(d).map(Some)
}

/// An envelope whose shape has been checked, before its method is typed.
enum Message<'a> {
    Request {
        id: u64,
        method: Cow<'a, str>,
        params: Option<Box<RawValue>>,
    },
    Notification {
        method: Cow<'a, str>,
        params: Option<Box<RawValue>>,
    },
    Result {
        id: u64,
        result: Box<RawValue>,
    },
    Error {
        id: u64,
        error: Box<RawValue>,
    },
}

fn read_envelope(line: &str) -> Result<Message<'_>, FrameError> {
    let envelope: Envelope<'_> = serde_json::from_str(line).map_err(malformed)?;
    if envelope.v != PROTOCOL_VERSION {
        return Err(FrameError::WrongVersion(envelope.v));
    }
    let Envelope {
        id,
        method,
        params,
        result,
        error,
        ..
    } = envelope;
    match (id, method, result, error) {
        (Some(id), Some(method), None, None) => Ok(Message::Request { id, method, params }),
        (None, Some(method), None, None) => Ok(Message::Notification { method, params }),
        (Some(id), None, Some(result), None) if params.is_none() => {
            Ok(Message::Result { id, result })
        }
        (Some(id), None, None, Some(error)) if params.is_none() => Ok(Message::Error { id, error }),
        _ => Err(FrameError::Malformed(
            "an envelope has exactly one of method, result or error, an id unless it is a \
             notification, and params only with a method"
                .to_string(),
        )),
    }
}

fn malformed(e: impl std::fmt::Display) -> FrameError {
    FrameError::Malformed(e.to_string())
}

/// A method's params, typed.
fn params<T: DeserializeOwned>(method: &str, raw: Option<&RawValue>) -> Result<T, FrameError> {
    let raw = raw.ok_or_else(|| FrameError::Malformed(format!("'{method}' needs params")))?;
    serde_json::from_str(raw.get()).map_err(|e| FrameError::Malformed(format!("{method}: {e}")))
}

/// A method that takes no params has none.
fn no_params(method: &str, raw: Option<&RawValue>) -> Result<(), FrameError> {
    match raw {
        None => Ok(()),
        Some(_) => Err(FrameError::Malformed(format!("'{method}' takes no params"))),
    }
}

#[derive(Serialize)]
struct OutRequest<'a, P> {
    v: u64,
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<P>,
}

#[derive(Serialize)]
struct OutNotification<'a, P> {
    v: u64,
    method: &'a str,
    params: P,
}

#[derive(Serialize)]
struct OutResult<R> {
    v: u64,
    id: u64,
    result: R,
}

#[derive(Serialize)]
struct OutError<'a> {
    v: u64,
    id: u64,
    error: &'a CallError,
}

/// A line, or only its length when a producer is measuring one.
enum Line {
    Text(String),
    Len(usize),
}

impl Line {
    fn text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            Self::Len(_) => None,
        }
    }
}

fn emit<T: Serialize>(measure: bool, out: &T) -> Result<Line, FrameError> {
    if measure {
        let mut count = Count(0);
        serde_json::to_writer(&mut count, out).map_err(malformed)?;
        Ok(Line::Len(count.0))
    } else {
        serde_json::to_string(out)
            .map(Line::Text)
            .map_err(malformed)
    }
}

/// Counts what is written to it and keeps none of it.
struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn request<P: Serialize>(
    measure: bool,
    id: u64,
    method: &str,
    params: Option<P>,
) -> Result<Line, FrameError> {
    let out = OutRequest {
        v: PROTOCOL_VERSION,
        id,
        method,
        params,
    };
    emit(measure, &out)
}

fn notification<P: Serialize>(measure: bool, method: &str, params: P) -> Result<Line, FrameError> {
    let out = OutNotification {
        v: PROTOCOL_VERSION,
        method,
        params,
    };
    emit(measure, &out)
}

fn result<R: Serialize>(measure: bool, id: u64, result: R) -> Result<Line, FrameError> {
    let out = OutResult {
        v: PROTOCOL_VERSION,
        id,
        result,
    };
    emit(measure, &out)
}

fn error(measure: bool, id: u64, error: &CallError) -> Result<Line, FrameError> {
    let out = OutError {
        v: PROTOCOL_VERSION,
        id,
        error,
    };
    emit(measure, &out)
}

// ---------------------------------------------------------------------------
// Methods, by direction
// ---------------------------------------------------------------------------

/// A request a worker may make.
#[derive(Debug)]
pub enum WorkerRequest {
    /// `session.hello`: the first message on a connection, and only then.
    SessionHello(HelloParams),
    /// `agent.invoke`
    AgentInvoke(InvokeAgentParams),
    /// `agent.list`
    AgentList,
    /// `mailbox.post`
    MailboxPost(SendMessageParams),
    /// `human.approve` (EN-2a): an approval only the root answers.
    HumanApprove(ApprovalRequest),
}

/// A notification a worker may send.
#[derive(Debug)]
pub enum WorkerNotification {
    /// `task.event`: progress on a `task.run`, named by its id.
    TaskEvent(TaskEvent<'static>),
    /// `req.cancel`: the worker withdraws one of its own requests.
    ReqCancel(IdParams),
    /// `task.input_required` (interim, *step 2*): a `task.run` parked on a
    /// question or an approval.
    TaskInputRequired(InputRequiredParams<'static>),
    /// `call.input` (interim, *step 2*): the answer to a question a callee
    /// of one of the worker's `agent.invoke` requests asked.
    CallInput(CallInputParams<'static>),
}

/// A request the broker may make.
#[derive(Debug)]
pub enum BrokerRequest {
    /// `task.run`: work.
    TaskRun(Box<TaskRunParams<'static>>),
}

/// A notification the broker may send.
#[derive(Debug)]
pub enum BrokerNotification {
    /// `req.progress`: progress on one of the worker's requests.
    ReqProgress(ProgressParams<'static>),
    /// `req.cancel`: the broker withdraws one of its `task.run`s.
    ReqCancel(IdParams),
    /// `task.input` (interim, *step 2*): the answer to a parked `task.run`.
    TaskInput(TaskInputParams<'static>),
    /// `call.input_required` (interim, *step 2*): a callee of one of the
    /// worker's requests parked on a question.
    CallInputRequired(CallQuestionParams<'static>),
    /// `call.input_withdrawn` (interim, *step 2*): that question is over.
    CallInputWithdrawn(CallTaskParams<'static>),
}

impl WorkerRequest {
    fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, FrameError> {
        Ok(match method {
            "session.hello" => Self::SessionHello(params(method, raw)?),
            "agent.invoke" => Self::AgentInvoke(params(method, raw)?),
            "agent.list" => {
                no_params(method, raw)?;
                Self::AgentList
            }
            "mailbox.post" => Self::MailboxPost(params(method, raw)?),
            "human.approve" => Self::HumanApprove(params(method, raw)?),
            other => return Err(FrameError::WrongDirection(other.to_string())),
        })
    }
}

impl WorkerNotification {
    fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, FrameError> {
        Ok(match method {
            "task.event" => Self::TaskEvent(params(method, raw)?),
            "req.cancel" => Self::ReqCancel(params(method, raw)?),
            "task.input_required" => Self::TaskInputRequired(params(method, raw)?),
            "call.input" => Self::CallInput(params(method, raw)?),
            other => return Err(FrameError::WrongDirection(other.to_string())),
        })
    }
}

impl BrokerRequest {
    fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, FrameError> {
        Ok(match method {
            "task.run" => Self::TaskRun(Box::new(params(method, raw)?)),
            other => return Err(FrameError::WrongDirection(other.to_string())),
        })
    }
}

impl BrokerNotification {
    fn decode(method: &str, raw: Option<&RawValue>) -> Result<Self, FrameError> {
        Ok(match method {
            "req.progress" => Self::ReqProgress(params(method, raw)?),
            "req.cancel" => Self::ReqCancel(params(method, raw)?),
            "task.input" => Self::TaskInput(params(method, raw)?),
            "call.input_required" => Self::CallInputRequired(params(method, raw)?),
            "call.input_withdrawn" => Self::CallInputWithdrawn(params(method, raw)?),
            other => return Err(FrameError::WrongDirection(other.to_string())),
        })
    }
}

/// `session.hello`'s params.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HelloParams {
    #[serde(default)]
    pub card: ParticipantCard,
}

/// `session.hello`'s result: who this connection is.
#[derive(Debug, Serialize, Deserialize)]
pub struct Welcome<'a> {
    pub name: Cow<'a, NodeName>,
    pub scope: Cow<'a, ConversationScope>,
    pub owner: Option<Cow<'a, NodeName>>,
}

/// A request id, for `req.cancel`.
#[derive(Debug, Serialize, Deserialize)]
pub struct IdParams {
    pub id: u64,
}

/// `task.event`: one update on a `task.run`, named by that request's id.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent<'a> {
    /// A non-terminal status. A terminal one is the `task.run`'s result.
    Status {
        id: u64,
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Cow<'a, Value>>,
    },
    /// A chunk of the task's output.
    #[serde(rename_all = "camelCase")]
    Artifact {
        id: u64,
        text: Cow<'a, str>,
        #[serde(default)]
        last_chunk: bool,
    },
    /// One of the worker's turns or tool events (TB-1).
    Swarm { id: u64, event: Cow<'a, SwarmItem> },
}

/// `task.run`'s result: how the task ended.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskOutcome<'a> {
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Cow<'a, Value>>,
}

/// `task.input_required`'s params.
#[derive(Debug, Serialize, Deserialize)]
pub struct InputRequiredParams<'a> {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Cow<'a, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Cow<'a, InputRequest>>,
}

/// `call.input`'s params: `id` is the worker's `agent.invoke`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CallInputParams<'a> {
    pub id: u64,
    pub task: Cow<'a, str>,
    pub input: Cow<'a, TaskInput>,
}

/// `task.run`'s params: today's task fields.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunParams<'a> {
    pub task_id: Cow<'a, str>,
    pub text: Cow<'a, str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer: Option<Cow<'a, TaskBearer>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub capture_conversation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_context: Option<Cow<'a, SpawnContext>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Cow<'a, HandoffContract>>,
    #[serde(default, skip_serializing_if = "is_unlimited")]
    pub budget: Cow<'a, Remaining>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub swarm_events: bool,
}

fn is_unlimited<B: std::ops::Deref<Target = Remaining>>(budget: &B) -> bool {
    budget.is_unlimited()
}

/// `req.progress`'s params.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProgressParams<'a> {
    pub id: u64,
    pub event: Cow<'a, Value>,
}

/// `task.input`'s params: `id` is the parked `task.run`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskInputParams<'a> {
    pub id: u64,
    pub input: Cow<'a, TaskInput>,
}

/// `call.input_required`'s params.
#[derive(Debug, Serialize, Deserialize)]
pub struct CallQuestionParams<'a> {
    pub id: u64,
    pub task: Cow<'a, str>,
    pub request: Cow<'a, Value>,
}

/// `call.input_withdrawn`'s params.
#[derive(Debug, Serialize, Deserialize)]
pub struct CallTaskParams<'a> {
    pub id: u64,
    pub task: Cow<'a, str>,
}

// ---------------------------------------------------------------------------
// The codec
// ---------------------------------------------------------------------------

/// The broker's end of a connection: encodes [`BrokerFrame`]s, decodes
/// [`ParticipantFrame`]s.
pub type BrokerCodec = FrameCodec<BrokerSide>;

/// A worker's end of a connection: encodes [`ParticipantFrame`]s, decodes
/// [`BrokerFrame`]s.
pub type WorkerCodec = FrameCodec<WorkerSide>;

/// One connection's codec; see the [module docs](self). Clones share state.
pub struct FrameCodec<S> {
    state: Arc<Mutex<State>>,
    side: PhantomData<fn() -> S>,
}

/// The broker's end.
pub enum BrokerSide {}
/// A worker's end.
pub enum WorkerSide {}

impl<S> Clone for FrameCodec<S> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            side: PhantomData,
        }
    }
}

impl<S> Default for FrameCodec<S> {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            side: PhantomData,
        }
    }
}

impl<S> FrameCodec<S> {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The id state of one connection, seen from one end.
#[derive(Default)]
struct State {
    /// The last id this end gave one of its own requests.
    last_id: u64,
    /// This end's `session.hello` (worker) or the peer's (broker), until it
    /// is answered.
    hello: Option<u64>,
    /// The peer's requests this end has not answered yet.
    theirs: HashSet<u64>,
    /// `task.run` request id → the A2A task id it carries. The broker's own
    /// requests on the broker's end, the broker's on the worker's.
    runs: HashMap<u64, String>,
    /// The reverse of `runs`.
    run_ids: HashMap<String, u64>,
    /// A worker's requests: request id → the call id its transport gave it.
    calls: HashMap<u64, u64>,
    /// The reverse of `calls`.
    call_ids: HashMap<u64, u64>,
    /// A worker's `human.approve` requests: request id → the approval
    /// number the worker gave it.
    approvals: HashMap<u64, u64>,
    /// The reverse of `approvals`.
    approval_ids: HashMap<u64, u64>,
    /// The broker's end: which of the peer's requests in flight are
    /// `human.approve`s, so a `req.cancel` withdraws an approval and only an
    /// approval is answered with a verdict.
    their_approvals: HashSet<u64>,
    /// How many messages this end has dropped, for rate-limited logging.
    dropped: u64,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// Note a request the peer made; a reused in-flight id is fatal.
    fn take_theirs(&mut self, id: u64) -> Result<(), FrameError> {
        if self.theirs.insert(id) {
            Ok(())
        } else {
            Err(FrameError::ReusedId(id))
        }
    }

    /// Drop a message naming nothing in flight. Logged on the 1st, 2nd,
    /// 4th, 8th… drop, so a peer cannot flood the log.
    fn drop_message(&mut self, what: &str, id: u64) {
        self.dropped += 1;
        if self.dropped.is_power_of_two() {
            warn!(
                what,
                id,
                dropped = self.dropped,
                "Dropped a message naming no request in flight"
            );
        }
    }

    fn open_run(&mut self, id: u64, task_id: &str) {
        self.runs.insert(id, task_id.to_string());
        self.run_ids.insert(task_id.to_string(), id);
    }

    fn close_run(&mut self, id: u64) -> Option<String> {
        let task_id = self.runs.remove(&id)?;
        self.run_ids.remove(&task_id);
        Some(task_id)
    }
}

impl FrameCodec<BrokerSide> {
    /// A frame for the worker as a line (no trailing newline), or `None`
    /// when it names nothing in flight and so is not sent.
    ///
    /// [`BrokerFrame::Error`] is sent only as the answer to a pending
    /// `session.hello`; with none, it is `None` and the caller just closes.
    pub fn encode(&self, frame: &BrokerFrame) -> Result<Option<String>, FrameError> {
        Ok(self.encode_line(frame, false)?.and_then(Line::text))
    }

    fn encode_line(&self, frame: &BrokerFrame, m: bool) -> Result<Option<Line>, FrameError> {
        let mut state = self.lock();
        let line = match frame {
            BrokerFrame::Welcome { name, scope, owner } => {
                let Some(id) = state.hello.take() else {
                    state.drop_message("welcome", 0);
                    return Ok(None);
                };
                state.theirs.remove(&id);
                let welcome = Welcome {
                    name: Cow::Borrowed(name),
                    scope: Cow::Borrowed(scope),
                    owner: owner.as_ref().map(Cow::Borrowed),
                };
                result(m, id, welcome)?
            }
            BrokerFrame::Error { reason } => {
                let Some(id) = state.hello.take() else {
                    return Ok(None);
                };
                state.theirs.remove(&id);
                error(m, id, &CallError::Refused(reason.clone()))?
            }
            BrokerFrame::Task {
                task_id,
                text,
                bearer,
                capture_conversation,
                spawn_context,
                handoff,
                budget,
                swarm_events,
            } => {
                let id = state.next_id();
                state.open_run(id, task_id);
                let params = TaskRunParams {
                    task_id: Cow::Borrowed(task_id),
                    text: Cow::Borrowed(text),
                    bearer: bearer.as_ref().map(Cow::Borrowed),
                    capture_conversation: *capture_conversation,
                    spawn_context: spawn_context.as_ref().map(Cow::Borrowed),
                    handoff: handoff.as_ref().map(Cow::Borrowed),
                    budget: Cow::Borrowed(&**budget),
                    swarm_events: *swarm_events,
                };
                request(m, id, "task.run", Some(params))?
            }
            BrokerFrame::Cancel { task_id } => {
                let Some(&id) = state.run_ids.get(task_id) else {
                    state.drop_message("req.cancel", 0);
                    return Ok(None);
                };
                notification(m, "req.cancel", IdParams { id })?
            }
            BrokerFrame::Input { task_id, input } => {
                let Some(&id) = state.run_ids.get(task_id) else {
                    state.drop_message("task.input", 0);
                    return Ok(None);
                };
                let params = TaskInputParams {
                    id,
                    input: Cow::Borrowed(input),
                };
                notification(m, "task.input", params)?
            }
            BrokerFrame::CallProgress { id, event } => {
                if !state.theirs.contains(id) {
                    state.drop_message("req.progress", *id);
                    return Ok(None);
                }
                let params = ProgressParams {
                    id: *id,
                    event: Cow::Borrowed(event),
                };
                notification(m, "req.progress", params)?
            }
            BrokerFrame::CallResult { id, result: value } => {
                if !state.theirs.remove(id) {
                    state.drop_message("result", *id);
                    return Ok(None);
                }
                result(m, *id, value)?
            }
            BrokerFrame::CallError { id, error: e } => {
                if !state.theirs.remove(id) {
                    state.drop_message("error", *id);
                    return Ok(None);
                }
                error(m, *id, e)?
            }
            BrokerFrame::CallInputRequired { id, task, request } => {
                if !state.theirs.contains(id) {
                    state.drop_message("call.input_required", *id);
                    return Ok(None);
                }
                let params = CallQuestionParams {
                    id: *id,
                    task: Cow::Borrowed(task),
                    request: Cow::Borrowed(request),
                };
                notification(m, "call.input_required", params)?
            }
            BrokerFrame::CallInputWithdrawn { id, task } => {
                if !state.theirs.contains(id) {
                    state.drop_message("call.input_withdrawn", *id);
                    return Ok(None);
                }
                let params = CallTaskParams {
                    id: *id,
                    task: Cow::Borrowed(task),
                };
                notification(m, "call.input_withdrawn", params)?
            }
            BrokerFrame::Approval { id, verdict } => {
                if !state.their_approvals.remove(id) {
                    state.drop_message("result", *id);
                    return Ok(None);
                }
                state.theirs.remove(id);
                result(m, *id, verdict)?
            }
        };
        Ok(Some(line))
    }

    /// One line from the worker as a frame, or `None` when it named nothing
    /// in flight and was dropped. An error closes the connection.
    pub fn decode(&self, line: &str) -> Result<Option<ParticipantFrame>, FrameError> {
        let message = read_envelope(line)?;
        let mut state = self.lock();
        let frame = match message {
            Message::Request { id, method, params } => {
                let request = WorkerRequest::decode(&method, params.as_deref())?;
                state.take_theirs(id)?;
                match request {
                    WorkerRequest::SessionHello(HelloParams { card }) => {
                        if state.hello.is_none() {
                            state.hello = Some(id);
                        }
                        ParticipantFrame::Hello { card }
                    }
                    WorkerRequest::AgentInvoke(params) => ParticipantFrame::Call {
                        id,
                        request: CallRequest::InvokeAgent(params),
                    },
                    WorkerRequest::AgentList => ParticipantFrame::Call {
                        id,
                        request: CallRequest::ListAgents,
                    },
                    WorkerRequest::MailboxPost(params) => ParticipantFrame::Call {
                        id,
                        request: CallRequest::SendMessage(params),
                    },
                    WorkerRequest::HumanApprove(request) => {
                        state.their_approvals.insert(id);
                        ParticipantFrame::Approve { id, request }
                    }
                }
            }
            Message::Notification { method, params } => {
                match WorkerNotification::decode(&method, params.as_deref())? {
                    WorkerNotification::TaskEvent(event) => {
                        let id = match &event {
                            TaskEvent::Status { id, .. }
                            | TaskEvent::Artifact { id, .. }
                            | TaskEvent::Swarm { id, .. } => *id,
                        };
                        let Some(task_id) = state.runs.get(&id).cloned() else {
                            state.drop_message("task.event", id);
                            return Ok(None);
                        };
                        match event {
                            TaskEvent::Status {
                                state: task_state,
                                message,
                                metadata,
                                ..
                            } => {
                                if task_state.is_terminal()
                                    || task_state == TaskState::InputRequired
                                {
                                    return Err(FrameError::Malformed(format!(
                                        "a task.event status is not '{task_state}'"
                                    )));
                                }
                                ParticipantFrame::Status {
                                    task_id,
                                    state: task_state,
                                    message: message.map(Cow::into_owned),
                                    metadata: metadata.map(Cow::into_owned),
                                    input: None,
                                }
                            }
                            TaskEvent::Artifact {
                                text, last_chunk, ..
                            } => ParticipantFrame::Artifact {
                                task_id,
                                text: text.into_owned(),
                                last_chunk,
                            },
                            TaskEvent::Swarm { event, .. } => ParticipantFrame::Event {
                                task_id,
                                event: event.into_owned(),
                            },
                        }
                    }
                    WorkerNotification::TaskInputRequired(params) => {
                        let Some(task_id) = state.runs.get(&params.id).cloned() else {
                            state.drop_message("task.input_required", params.id);
                            return Ok(None);
                        };
                        ParticipantFrame::Status {
                            task_id,
                            state: TaskState::InputRequired,
                            message: params.message.map(Cow::into_owned),
                            metadata: params.metadata.map(Cow::into_owned),
                            input: params.input.map(Cow::into_owned),
                        }
                    }
                    WorkerNotification::ReqCancel(IdParams { id }) => {
                        if !state.theirs.remove(&id) {
                            state.drop_message("req.cancel", id);
                            return Ok(None);
                        }
                        if state.their_approvals.remove(&id) {
                            ParticipantFrame::CancelApproval { id }
                        } else {
                            ParticipantFrame::CancelCall { id }
                        }
                    }
                    WorkerNotification::CallInput(params) => {
                        if !state.theirs.contains(&params.id) {
                            state.drop_message("call.input", params.id);
                            return Ok(None);
                        }
                        ParticipantFrame::CallInput {
                            id: params.id,
                            task: params.task.into_owned(),
                            input: params.input.into_owned(),
                        }
                    }
                }
            }
            Message::Result { id, result } => {
                let Some(task_id) = state.close_run(id) else {
                    state.drop_message("result", id);
                    return Ok(None);
                };
                let outcome: TaskOutcome<'_> =
                    serde_json::from_str(result.get()).map_err(malformed)?;
                if !outcome.state.is_terminal() {
                    return Err(FrameError::Malformed(format!(
                        "a task.run result is terminal, not '{}'",
                        outcome.state
                    )));
                }
                ParticipantFrame::Status {
                    task_id,
                    state: outcome.state,
                    message: outcome.message.map(Cow::into_owned),
                    metadata: outcome.metadata.map(Cow::into_owned),
                    input: None,
                }
            }
            Message::Error { id, error } => {
                let Some(task_id) = state.close_run(id) else {
                    state.drop_message("error", id);
                    return Ok(None);
                };
                let error: CallError = serde_json::from_str(error.get()).map_err(malformed)?;
                ParticipantFrame::Status {
                    task_id,
                    state: TaskState::Failed,
                    message: Some(error.to_string()),
                    metadata: None,
                    input: None,
                }
            }
        };
        Ok(Some(frame))
    }

    /// An upper bound on the length of `frame`'s line, without touching the
    /// connection's state: what a producer checks against the frame cap
    /// before it queues the frame.
    pub fn line_len_bound(frame: &BrokerFrame) -> usize {
        // The largest id a line can carry; ids in a real line are shorter.
        let scratch = FrameCodec::<BrokerSide>::new();
        {
            let mut state = scratch.lock();
            state.hello = Some(u64::MAX);
            state.last_id = u64::MAX - 1;
            match frame {
                BrokerFrame::Cancel { task_id } | BrokerFrame::Input { task_id, .. } => {
                    state.open_run(u64::MAX, task_id)
                }
                BrokerFrame::CallProgress { id, .. }
                | BrokerFrame::CallResult { id, .. }
                | BrokerFrame::CallError { id, .. }
                | BrokerFrame::CallInputRequired { id, .. }
                | BrokerFrame::CallInputWithdrawn { id, .. } => {
                    state.theirs.insert(*id);
                }
                BrokerFrame::Approval { id, .. } => {
                    state.theirs.insert(*id);
                    state.their_approvals.insert(*id);
                }
                BrokerFrame::Welcome { .. }
                | BrokerFrame::Error { .. }
                | BrokerFrame::Task { .. } => {}
            }
        }
        // A frame that cannot be encoded is not sent either; it counts as
        // over any cap.
        match scratch.encode_line(frame, true) {
            Ok(Some(Line::Len(len))) => len,
            Ok(Some(Line::Text(line))) => line.len(),
            Ok(None) | Err(_) => usize::MAX,
        }
    }
}

impl FrameCodec<WorkerSide> {
    /// A frame for the broker as a line (no trailing newline), or `None`
    /// when it names nothing in flight and so is not sent.
    pub fn encode(&self, frame: &ParticipantFrame) -> Result<Option<String>, FrameError> {
        let m = false;
        let mut state = self.lock();
        let line = match frame {
            ParticipantFrame::Hello { card } => {
                let id = state.next_id();
                state.hello = Some(id);
                request(m, id, "session.hello", Some(HelloParamsRef { card }))?
            }
            ParticipantFrame::Status {
                task_id,
                state: task_state,
                message,
                metadata,
                input,
            } => {
                let Some(&id) = state.run_ids.get(task_id) else {
                    state.drop_message("status", 0);
                    return Ok(None);
                };
                let message = message.as_deref().map(Cow::Borrowed);
                let metadata = metadata.as_ref().map(Cow::Borrowed);
                if task_state.is_terminal() {
                    state.close_run(id);
                    state.theirs.remove(&id);
                    let outcome = TaskOutcome {
                        state: *task_state,
                        message,
                        metadata,
                    };
                    result(m, id, outcome)?
                } else if *task_state == TaskState::InputRequired {
                    let params = InputRequiredParams {
                        id,
                        message,
                        metadata,
                        input: input.as_ref().map(Cow::Borrowed),
                    };
                    notification(m, "task.input_required", params)?
                } else {
                    let event = TaskEvent::Status {
                        id,
                        state: *task_state,
                        message,
                        metadata,
                    };
                    notification(m, "task.event", event)?
                }
            }
            ParticipantFrame::Artifact {
                task_id,
                text,
                last_chunk,
            } => {
                let Some(&id) = state.run_ids.get(task_id) else {
                    state.drop_message("artifact", 0);
                    return Ok(None);
                };
                let event = TaskEvent::Artifact {
                    id,
                    text: Cow::Borrowed(text),
                    last_chunk: *last_chunk,
                };
                notification(m, "task.event", event)?
            }
            ParticipantFrame::Event { task_id, event } => {
                let Some(&id) = state.run_ids.get(task_id) else {
                    state.drop_message("event", 0);
                    return Ok(None);
                };
                let event = TaskEvent::Swarm {
                    id,
                    event: Cow::Borrowed(event),
                };
                notification(m, "task.event", event)?
            }
            ParticipantFrame::Call {
                id: call,
                request: call_request,
            } => {
                let id = state.next_id();
                state.calls.insert(id, *call);
                state.call_ids.insert(*call, id);
                match call_request {
                    CallRequest::InvokeAgent(params) => {
                        request(m, id, "agent.invoke", Some(params))?
                    }
                    CallRequest::ListAgents => request::<()>(m, id, "agent.list", None)?,
                    CallRequest::SendMessage(params) => {
                        request(m, id, "mailbox.post", Some(params))?
                    }
                }
            }
            ParticipantFrame::CallInput {
                id: call,
                task,
                input,
            } => {
                let Some(&id) = state.call_ids.get(call) else {
                    state.drop_message("call.input", *call);
                    return Ok(None);
                };
                let params = CallInputParams {
                    id,
                    task: Cow::Borrowed(task),
                    input: Cow::Borrowed(input),
                };
                notification(m, "call.input", params)?
            }
            ParticipantFrame::CancelCall { id: call } => {
                // A request this end cancelled is withdrawn: whatever the
                // broker still sends for it is dropped.
                let Some(id) = state.call_ids.remove(call) else {
                    state.drop_message("req.cancel", *call);
                    return Ok(None);
                };
                state.calls.remove(&id);
                notification(m, "req.cancel", IdParams { id })?
            }
            ParticipantFrame::Approve {
                id: approval,
                request: approve,
            } => {
                let id = state.next_id();
                state.approvals.insert(id, *approval);
                state.approval_ids.insert(*approval, id);
                request(m, id, "human.approve", Some(approve))?
            }
            ParticipantFrame::CancelApproval { id: approval } => {
                // Answered already, or never sent: nothing to withdraw.
                let Some(id) = state.approval_ids.remove(approval) else {
                    state.drop_message("req.cancel", *approval);
                    return Ok(None);
                };
                state.approvals.remove(&id);
                notification(m, "req.cancel", IdParams { id })?
            }
        };
        Ok(line.text())
    }

    /// One line from the broker as a frame, or `None` when it named nothing
    /// in flight and was dropped. An error closes the connection.
    pub fn decode(&self, line: &str) -> Result<Option<BrokerFrame>, FrameError> {
        let message = read_envelope(line)?;
        let mut state = self.lock();
        let frame = match message {
            Message::Request { id, method, params } => {
                match BrokerRequest::decode(&method, params.as_deref())? {
                    BrokerRequest::TaskRun(params) => {
                        state.take_theirs(id)?;
                        let TaskRunParams {
                            task_id,
                            text,
                            bearer,
                            capture_conversation,
                            spawn_context,
                            handoff,
                            budget,
                            swarm_events,
                        } = *params;
                        let task_id = task_id.into_owned();
                        state.open_run(id, &task_id);
                        BrokerFrame::Task {
                            task_id,
                            text: text.into_owned(),
                            bearer: bearer.map(Cow::into_owned),
                            capture_conversation,
                            spawn_context: spawn_context.map(Cow::into_owned),
                            handoff: handoff.map(Cow::into_owned),
                            budget: Box::new(budget.into_owned()),
                            swarm_events,
                        }
                    }
                }
            }
            Message::Notification { method, params } => {
                match BrokerNotification::decode(&method, params.as_deref())? {
                    BrokerNotification::ReqProgress(params) => {
                        let Some(&call) = state.calls.get(&params.id) else {
                            state.drop_message("req.progress", params.id);
                            return Ok(None);
                        };
                        BrokerFrame::CallProgress {
                            id: call,
                            event: params.event.into_owned(),
                        }
                    }
                    BrokerNotification::ReqCancel(IdParams { id }) => {
                        let Some(task_id) = state.runs.get(&id).cloned() else {
                            state.drop_message("req.cancel", id);
                            return Ok(None);
                        };
                        BrokerFrame::Cancel { task_id }
                    }
                    BrokerNotification::TaskInput(params) => {
                        let Some(task_id) = state.runs.get(&params.id).cloned() else {
                            state.drop_message("task.input", params.id);
                            return Ok(None);
                        };
                        BrokerFrame::Input {
                            task_id,
                            input: params.input.into_owned(),
                        }
                    }
                    BrokerNotification::CallInputRequired(params) => {
                        let Some(&call) = state.calls.get(&params.id) else {
                            state.drop_message("call.input_required", params.id);
                            return Ok(None);
                        };
                        BrokerFrame::CallInputRequired {
                            id: call,
                            task: params.task.into_owned(),
                            request: params.request.into_owned(),
                        }
                    }
                    BrokerNotification::CallInputWithdrawn(params) => {
                        let Some(&call) = state.calls.get(&params.id) else {
                            state.drop_message("call.input_withdrawn", params.id);
                            return Ok(None);
                        };
                        BrokerFrame::CallInputWithdrawn {
                            id: call,
                            task: params.task.into_owned(),
                        }
                    }
                }
            }
            Message::Result { id, result } => {
                if state.hello == Some(id) {
                    state.hello = None;
                    let welcome: Welcome<'_> =
                        serde_json::from_str(result.get()).map_err(malformed)?;
                    BrokerFrame::Welcome {
                        name: welcome.name.into_owned(),
                        scope: welcome.scope.into_owned(),
                        owner: welcome.owner.map(Cow::into_owned),
                    }
                } else if let Some(call) = state.calls.remove(&id) {
                    state.call_ids.remove(&call);
                    let result: Value = serde_json::from_str(result.get()).map_err(malformed)?;
                    BrokerFrame::CallResult { id: call, result }
                } else if let Some(approval) = state.approvals.remove(&id) {
                    state.approval_ids.remove(&approval);
                    let verdict: ApprovalVerdict =
                        serde_json::from_str(result.get()).map_err(malformed)?;
                    BrokerFrame::Approval {
                        id: approval,
                        verdict,
                    }
                } else {
                    state.drop_message("result", id);
                    return Ok(None);
                }
            }
            Message::Error { id, error } => {
                let error: CallError = serde_json::from_str(error.get()).map_err(malformed)?;
                if state.hello == Some(id) {
                    state.hello = None;
                    let reason = match error {
                        CallError::Refused(reason) => reason,
                        other => other.to_string(),
                    };
                    BrokerFrame::Error { reason }
                } else if let Some(call) = state.calls.remove(&id) {
                    state.call_ids.remove(&call);
                    BrokerFrame::CallError { id: call, error }
                } else if let Some(approval) = state.approvals.remove(&id) {
                    // An approval nobody granted is denied.
                    state.approval_ids.remove(&approval);
                    warn!(approval, %error, "The broker failed an approval; denying it");
                    BrokerFrame::Approval {
                        id: approval,
                        verdict: ApprovalVerdict::Denied,
                    }
                } else {
                    state.drop_message("error", id);
                    return Ok(None);
                }
            }
        };
        Ok(Some(frame))
    }
}

/// `session.hello`'s params, borrowed for encoding.
#[derive(Serialize)]
struct HelloParamsRef<'a> {
    card: &'a ParticipantCard,
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
