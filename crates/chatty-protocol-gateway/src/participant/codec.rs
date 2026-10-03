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
//! separate enums, defined with every param, result and error type in
//! [`chatty_fabric::wire`]. A method the peer may not send does not decode;
//! neither does a field a type does not have, a duplicate key, or an error
//! `kind` outside [`WireError`]. A decode error closes the connection.
//!
//! | Sender | Requests | Notifications |
//! |---|---|---|
//! | worker | `session.hello`, `agent.invoke`, `agent.list`, `mailbox.post`, `human.ask`, `human.approve` | `task.event`, `req.cancel` |
//! | broker | `task.run`, `human.ask` | `req.progress`, `req.cancel` |
//!
//! `human.approve` (EN-2a) is a worker's request only: the broker answers
//! it with the root's verdict and never sends one, so a worker that is sent
//! one closes the connection. `human.ask` (EN-2b) goes both ways: a worker
//! asks a question with it, and the broker relays a callee's question to
//! its caller with it, under the broker's own id for the question
//! (`question`), and the caller's result is the answers or `escalate`.
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

use chatty_fabric::wire::{
    AgentEntry, BrokerNotification, BrokerRequest, DecodeError, HelloParams, IdParams,
    ProgressParams, RelayedAskParams, TaskEvent, TaskOutcome, TaskRunParams, Welcome, WireError,
    WorkerNotification, WorkerRequest,
};
use chatty_fabric::{
    Answer, ApprovalVerdict, AskReply, CallError, CallRequest, CallResult, InvokeAgentOutcome,
    MessageStatus,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use tracing::{debug, warn};

use super::protocol::{BrokerFrame, ParticipantCard, ParticipantFrame, TaskState};

pub use chatty_fabric::wire::PROTOCOL_VERSION;

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

impl From<DecodeError> for FrameError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Malformed(reason) => Self::Malformed(reason),
            DecodeError::WrongDirection(method) => Self::WrongDirection(method),
        }
    }
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

/// A result or an error, typed: decoded straight from its JSON text.
fn typed<T: DeserializeOwned>(what: &str, raw: &RawValue) -> Result<T, FrameError> {
    serde_json::from_str(raw.get()).map_err(|e| FrameError::Malformed(format!("{what}: {e}")))
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
    error: &'a WireError,
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
        error: &WireError::from(error.clone()),
    };
    emit(measure, &out)
}

/// Which method a worker's call is, so its result decodes as that
/// method's result type.
#[derive(Debug, Clone, Copy)]
enum CallMethod {
    Invoke,
    List,
    Post,
    Take,
}

impl CallMethod {
    fn of(request: &CallRequest) -> Self {
        match request {
            CallRequest::InvokeAgent(_) => Self::Invoke,
            CallRequest::ListAgents => Self::List,
            CallRequest::SendMessage(_) => Self::Post,
            CallRequest::TakeMessages => Self::Take,
        }
    }

    fn result(self, raw: &RawValue) -> Result<CallResult, FrameError> {
        Ok(match self {
            Self::Invoke => CallResult::Invoked(typed::<InvokeAgentOutcome>("agent.invoke", raw)?),
            Self::List => CallResult::Agents(typed::<Vec<AgentEntry>>("agent.list", raw)?),
            Self::Post => CallResult::Posted(typed::<MessageStatus>("mailbox.post", raw)?),
            Self::Take => CallResult::Messages(typed::<Vec<String>>("mailbox.take", raw)?),
        })
    }
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
    /// A worker's requests: request id → the call id its transport gave it,
    /// and which method it is.
    calls: HashMap<u64, (u64, CallMethod)>,
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
    /// A worker's `human.ask` requests: request id → the question number
    /// the worker gave it.
    asks: HashMap<u64, u64>,
    /// The reverse of `asks`.
    ask_ids: HashMap<u64, u64>,
    /// The broker's end: which of the peer's requests in flight are
    /// `human.ask`s, so a `req.cancel` withdraws a question and only a
    /// question is answered with answers.
    their_asks: HashSet<u64>,
    /// The broker's relayed `human.ask`s in flight: request id → the
    /// broker's id for the question. The broker's own requests on the
    /// broker's end, the broker's on the worker's.
    relayed: HashMap<u64, String>,
    /// The reverse of `relayed`.
    relayed_ids: HashMap<String, u64>,
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

    fn open_relay(&mut self, id: u64, question: &str) {
        self.relayed.insert(id, question.to_string());
        self.relayed_ids.insert(question.to_string(), id);
    }

    fn close_relay_by_question(&mut self, question: &str) -> Option<u64> {
        let id = self.relayed_ids.remove(question)?;
        self.relayed.remove(&id);
        Some(id)
    }

    fn close_relay(&mut self, id: u64) -> Option<String> {
        let question = self.relayed.remove(&id)?;
        self.relayed_ids.remove(&question);
        Some(question)
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
            BrokerFrame::SchemaMismatch { reason } => {
                let Some(id) = state.hello.take() else {
                    return Ok(None);
                };
                state.theirs.remove(&id);
                error(m, id, &CallError::Protocol(reason.clone()))?
            }
            BrokerFrame::Task {
                task_id,
                text,
                identity,
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
                    identity: identity.as_ref().map(Cow::Borrowed),
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
            BrokerFrame::Approval { id, verdict } => {
                if !state.their_approvals.remove(id) {
                    state.drop_message("result", *id);
                    return Ok(None);
                }
                state.theirs.remove(id);
                result(m, *id, verdict)?
            }
            BrokerFrame::Answer { id, answers } => {
                if !state.their_asks.remove(id) {
                    state.drop_message("result", *id);
                    return Ok(None);
                }
                state.theirs.remove(id);
                match answers {
                    Ok(answers) => result(m, *id, answers)?,
                    Err(e) => error(m, *id, e)?,
                }
            }
            BrokerFrame::Ask {
                question,
                request: ask,
            } => {
                let id = state.next_id();
                state.open_relay(id, question);
                let params = RelayedAskParams {
                    question: Cow::Borrowed(question),
                    request: Cow::Borrowed(ask),
                };
                request(m, id, "human.ask", Some(params))?
            }
            BrokerFrame::CancelAsk { question } => {
                // Answered already, or never sent: nothing to withdraw.
                let Some(id) = state.relayed_ids.get(question).copied() else {
                    state.drop_message("req.cancel", 0);
                    return Ok(None);
                };
                state.close_relay(id);
                notification(m, "req.cancel", IdParams { id })?
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
                    WorkerRequest::SessionHello(HelloParams { card, schema }) => {
                        if state.hello.is_none() {
                            state.hello = Some(id);
                        }
                        ParticipantFrame::Hello { card, schema }
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
                    WorkerRequest::MailboxTake => ParticipantFrame::Call {
                        id,
                        request: CallRequest::TakeMessages,
                    },
                    WorkerRequest::HumanApprove(request) => {
                        state.their_approvals.insert(id);
                        ParticipantFrame::Approve { id, request }
                    }
                    WorkerRequest::HumanAsk(request) => {
                        state.their_asks.insert(id);
                        ParticipantFrame::Ask { id, request }
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
                                    metadata: None,
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
                    WorkerNotification::ReqCancel(IdParams { id }) => {
                        if !state.theirs.remove(&id) {
                            state.drop_message("req.cancel", id);
                            return Ok(None);
                        }
                        if state.their_approvals.remove(&id) {
                            ParticipantFrame::CancelApproval { id }
                        } else if state.their_asks.remove(&id) {
                            ParticipantFrame::CancelAsk { id }
                        } else {
                            ParticipantFrame::CancelCall { id }
                        }
                    }
                }
            }
            Message::Result { id, result } => {
                if let Some(question) = state.close_relay(id) {
                    let reply: AskReply = typed("human.ask result", &result)?;
                    return Ok(Some(ParticipantFrame::AskReply { question, reply }));
                }
                let Some(task_id) = state.close_run(id) else {
                    state.drop_message("result", id);
                    return Ok(None);
                };
                let outcome: TaskOutcome<'_> = typed("task.run result", &result)?;
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
                }
            }
            Message::Error { id, error } => {
                let error = CallError::from(typed::<WireError>("error", &error)?);
                if let Some(question) = state.close_relay(id) {
                    // A caller that could not answer passes it on.
                    debug!(%question, %error, "A caller failed a relayed question; escalating it");
                    return Ok(Some(ParticipantFrame::AskReply {
                        question,
                        reply: AskReply::Escalate,
                    }));
                }
                let Some(task_id) = state.close_run(id) else {
                    state.drop_message("error", id);
                    return Ok(None);
                };
                ParticipantFrame::Status {
                    task_id,
                    state: TaskState::Failed,
                    message: Some(error.to_string()),
                    metadata: None,
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
                BrokerFrame::Cancel { task_id } => state.open_run(u64::MAX, task_id),
                BrokerFrame::CancelAsk { question } => state.open_relay(u64::MAX, question),
                BrokerFrame::CallProgress { id, .. }
                | BrokerFrame::CallResult { id, .. }
                | BrokerFrame::CallError { id, .. } => {
                    state.theirs.insert(*id);
                }
                BrokerFrame::Approval { id, .. } => {
                    state.theirs.insert(*id);
                    state.their_approvals.insert(*id);
                }
                BrokerFrame::Answer { id, .. } => {
                    state.theirs.insert(*id);
                    state.their_asks.insert(*id);
                }
                BrokerFrame::Welcome { .. }
                | BrokerFrame::Error { .. }
                | BrokerFrame::SchemaMismatch { .. }
                | BrokerFrame::Task { .. }
                | BrokerFrame::Ask { .. } => {}
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
            ParticipantFrame::Hello { card, schema } => {
                let id = state.next_id();
                state.hello = Some(id);
                request(
                    m,
                    id,
                    "session.hello",
                    Some(HelloParamsRef { card, schema }),
                )?
            }
            ParticipantFrame::Status {
                task_id,
                state: task_state,
                message,
                metadata,
            } => {
                let Some(&id) = state.run_ids.get(task_id) else {
                    state.drop_message("status", 0);
                    return Ok(None);
                };
                let message = message.as_deref().map(Cow::Borrowed);
                if task_state.is_terminal() {
                    let metadata = metadata.as_ref().map(Cow::Borrowed);
                    state.close_run(id);
                    state.theirs.remove(&id);
                    let outcome = TaskOutcome {
                        state: *task_state,
                        message,
                        metadata,
                    };
                    result(m, id, outcome)?
                } else if *task_state == TaskState::InputRequired {
                    // A question is a `human.ask` request (EN-2b).
                    return Err(FrameError::Malformed(
                        "a task waits on a human with human.ask, not an input-required status"
                            .to_string(),
                    ));
                } else if metadata.is_some() {
                    return Err(FrameError::Malformed(format!(
                        "a '{task_state}' status carries no metadata; only a terminal one does"
                    )));
                } else {
                    let event = TaskEvent::Status {
                        id,
                        state: *task_state,
                        message,
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
                state
                    .calls
                    .insert(id, (*call, CallMethod::of(call_request)));
                state.call_ids.insert(*call, id);
                match call_request {
                    CallRequest::InvokeAgent(params) => {
                        request(m, id, "agent.invoke", Some(params))?
                    }
                    CallRequest::ListAgents => request::<()>(m, id, "agent.list", None)?,
                    CallRequest::SendMessage(params) => {
                        request(m, id, "mailbox.post", Some(params))?
                    }
                    CallRequest::TakeMessages => request::<()>(m, id, "mailbox.take", None)?,
                }
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
            ParticipantFrame::Ask {
                id: question,
                request: ask,
            } => {
                let id = state.next_id();
                state.asks.insert(id, *question);
                state.ask_ids.insert(*question, id);
                request(m, id, "human.ask", Some(ask))?
            }
            ParticipantFrame::CancelAsk { id: question } => {
                // Answered already, or never sent: nothing to withdraw.
                let Some(id) = state.ask_ids.remove(question) else {
                    state.drop_message("req.cancel", *question);
                    return Ok(None);
                };
                state.asks.remove(&id);
                notification(m, "req.cancel", IdParams { id })?
            }
            ParticipantFrame::AskReply { question, reply } => {
                // Withdrawn already: nobody is waiting on the reply.
                let Some(id) = state.close_relay_by_question(question) else {
                    state.drop_message("result", 0);
                    return Ok(None);
                };
                state.theirs.remove(&id);
                result(m, id, reply)?
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
                            identity,
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
                            identity: identity.map(Cow::into_owned),
                            capture_conversation,
                            spawn_context: spawn_context.map(Cow::into_owned),
                            handoff: handoff.map(Cow::into_owned),
                            budget: Box::new(budget.into_owned()),
                            swarm_events,
                        }
                    }
                    BrokerRequest::HumanAsk(params) => {
                        state.take_theirs(id)?;
                        let question = params.question.into_owned();
                        state.open_relay(id, &question);
                        BrokerFrame::Ask {
                            question,
                            request: params.request.into_owned(),
                        }
                    }
                }
            }
            Message::Notification { method, params } => {
                match BrokerNotification::decode(&method, params.as_deref())? {
                    BrokerNotification::ReqProgress(params) => {
                        let Some(&(call, _)) = state.calls.get(&params.id) else {
                            state.drop_message("req.progress", params.id);
                            return Ok(None);
                        };
                        BrokerFrame::CallProgress {
                            id: call,
                            event: params.event.into_owned(),
                        }
                    }
                    BrokerNotification::ReqCancel(IdParams { id }) => {
                        if let Some(task_id) = state.runs.get(&id).cloned() {
                            BrokerFrame::Cancel { task_id }
                        } else if let Some(question) = state.close_relay(id) {
                            // A request the broker cancelled is withdrawn:
                            // the worker sends no result for it.
                            state.theirs.remove(&id);
                            BrokerFrame::CancelAsk { question }
                        } else {
                            state.drop_message("req.cancel", id);
                            return Ok(None);
                        }
                    }
                }
            }
            Message::Result { id, result } => {
                if state.hello == Some(id) {
                    state.hello = None;
                    let welcome: Welcome<'_> = typed("session.hello result", &result)?;
                    BrokerFrame::Welcome {
                        name: welcome.name.into_owned(),
                        scope: welcome.scope.into_owned(),
                        owner: welcome.owner.map(Cow::into_owned),
                    }
                } else if let Some((call, method)) = state.calls.remove(&id) {
                    state.call_ids.remove(&call);
                    BrokerFrame::CallResult {
                        id: call,
                        result: method.result(&result)?,
                    }
                } else if let Some(approval) = state.approvals.remove(&id) {
                    state.approval_ids.remove(&approval);
                    let verdict: ApprovalVerdict = typed("human.approve result", &result)?;
                    BrokerFrame::Approval {
                        id: approval,
                        verdict,
                    }
                } else if let Some(question) = state.asks.remove(&id) {
                    state.ask_ids.remove(&question);
                    let answers: Vec<Answer> = typed("human.ask result", &result)?;
                    BrokerFrame::Answer {
                        id: question,
                        answers: Ok(answers),
                    }
                } else {
                    state.drop_message("result", id);
                    return Ok(None);
                }
            }
            Message::Error { id, error } => {
                let error = CallError::from(typed::<WireError>("error", &error)?);
                if state.hello == Some(id) {
                    state.hello = None;
                    let reason = match error {
                        CallError::Refused(reason) => reason,
                        other => other.to_string(),
                    };
                    BrokerFrame::Error { reason }
                } else if let Some((call, _)) = state.calls.remove(&id) {
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
                } else if let Some(question) = state.asks.remove(&id) {
                    state.ask_ids.remove(&question);
                    BrokerFrame::Answer {
                        id: question,
                        answers: Err(error),
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
    schema: &'a str,
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
