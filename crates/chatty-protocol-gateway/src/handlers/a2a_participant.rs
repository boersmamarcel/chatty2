//! Serving a local participant — registered or spawned — over the gateway's
//! A2A surface.
//!
//! A participant's task updates ([`TaskUpdate`]) become the same JSON-RPC and
//! SSE frames a WASM module's do, so `A2aClient` — and anything else speaking
//! A2A — cannot tell the two apart. That equivalence is the point: ADR-0011's
//! first kill criterion asks whether a delegated turn survives the round trip
//! at the granularity the parent already renders, and it can only be answered
//! if the wire shape is the one the parent already reads.
//!
//! Two things arrive here. A **registered participant** is a process that
//! connected on its own; a task is submitted straight to it. A **runner
//! agent** (ADR-0011 C2) has no process yet: the task spawns one, waits for
//! it to register, and reaps it afterwards. Past the first step the two are
//! the same code, which is why a caller cannot tell those apart either.

use axum::{
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::participant::{
    DelegatedTask, InputRequest, ParticipantCard, ParticipantRegistry, TaskInput, TaskState,
    TaskStream, TaskUpdate, VirtualAgent, WorkerHandle,
};

use super::jsonrpc::{INTERNAL_ERROR, INVALID_PARAMS, json_rpc_error, json_rpc_ok};

/// The key under an A2A status's `metadata` that carries what a parked task
/// is waiting for, and under a `message/send` message's `metadata` that
/// carries the answer. A2A's own `TaskStatus` has no field for either;
/// `metadata` is the extension point it offers (ADR-0011 C7, AGE-306).
pub const CLARIFICATION_METADATA_KEY: &str = "clarification";

/// The failure a non-streaming caller gets when its worker asks something.
///
/// It quotes the question, because "the delegation failed" would leave the
/// caller with no idea that anything was asked, which is the whole complaint
/// in AGE-321. `status.message` carries the worker's own phrasing when it has
/// one; the structured request stays on `status.metadata` for a caller that
/// wants to parse it.
fn unanswerable_question(worker_message: Option<&str>, metadata: Option<&Value>) -> String {
    let asked = worker_message
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| questions_from(metadata))
        .unwrap_or_else(|| "a question".to_string());

    format!(
        "the worker asked: {asked} — a `message/send` task cannot carry a \
         question back to its caller, so it cannot be answered. Use \
         `message/stream`, which carries `input-required` and takes the \
         answer on the same task."
    )
}

/// Every question in a parked task's `metadata.clarification`, joined.
fn questions_from(metadata: Option<&Value>) -> Option<String> {
    let questions = metadata?
        .get(CLARIFICATION_METADATA_KEY)?
        .get("questions")?
        .as_array()?;
    let asked: Vec<String> = questions
        .iter()
        .filter_map(|q| q.get("question")?.as_str().map(str::to_string))
        .collect();
    (!asked.is_empty()).then(|| asked.join("; "))
}

// ---------------------------------------------------------------------------
// Agent card
// ---------------------------------------------------------------------------

/// A participant's card in the same JSON shape as a module's.
pub(crate) fn card_to_json(card: &ParticipantCard) -> Value {
    let skills: Vec<Value> = card
        .skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "examples": s.examples,
            })
        })
        .collect();

    json!({
        "name": card.name,
        "displayName": card.display_name.clone().unwrap_or_else(|| card.name.clone()),
        "description": card.description,
        "version": card.version,
        "skills": skills,
        "capabilities": { "streaming": true },
    })
}

// ---------------------------------------------------------------------------
// One task in flight
// ---------------------------------------------------------------------------

/// A submitted task and everything that has to be cleaned up after it.
struct RunningTask {
    task_id: String,
    updates: TaskStream,
    /// Cancels the task if the caller hangs up before it finishes.
    guard: TaskGuard,
    /// Present only for a worker a virtual agent started: the process or the
    /// leased microVM behind it, reaped when this is dropped.
    worker: Option<Box<dyn WorkerHandle>>,
}

impl RunningTask {
    /// The task reached a terminal state; nothing is left to cancel.
    ///
    /// `metadata` is the terminal status's, carrying the worker's token
    /// usage. It is passed on rather than only rendered because a hosted
    /// worker's ledger row wants it beside the lease-seconds only the worker
    /// handle knows (AGE-307).
    fn finish(&mut self, succeeded: bool, metadata: Option<&Value>) {
        self.guard.finished();
        if let Some(worker) = self.worker.as_mut() {
            worker.finish(succeeded, metadata);
        }
    }
}

/// Submit `task` to an already-registered participant.
fn submit(registry: &ParticipantRegistry, name: &str, task: DelegatedTask) -> Option<RunningTask> {
    let (task_id, updates) = registry.submit_task(name, task)?;
    Some(RunningTask {
        guard: TaskGuard::new(registry.clone(), name.to_string(), task_id.clone()),
        task_id,
        updates,
        worker: None,
    })
}

/// Start a worker for `task` and submit it.
async fn spawn(runner: &dyn VirtualAgent, task: DelegatedTask) -> Result<RunningTask, String> {
    let (worker, updates) = runner.run_task(task).await.map_err(|e| format!("{e:#}"))?;
    let task_id = worker
        .task_id()
        .expect("run_task sets the task id before returning")
        .to_string();
    Ok(RunningTask {
        guard: TaskGuard::new(
            runner.registry().clone(),
            worker.name().to_string(),
            task_id.clone(),
        ),
        task_id,
        updates,
        worker: Some(worker),
    })
}

// ---------------------------------------------------------------------------
// message/send
// ---------------------------------------------------------------------------

/// Run one task on a registered participant to completion.
pub(crate) async fn message_send(
    registry: &ParticipantRegistry,
    name: &str,
    id: Option<Value>,
    task: DelegatedTask,
) -> Response {
    match submit(registry, name, task) {
        Some(task) => send_task(id, task).await,
        None => json_rpc_error(
            StatusCode::OK,
            id,
            INTERNAL_ERROR,
            format!("participant '{name}' is no longer connected"),
        ),
    }
}

/// Start a worker, run one task on it to completion, and reap it.
pub(crate) async fn runner_message_send(
    runner: &dyn VirtualAgent,
    id: Option<Value>,
    task: DelegatedTask,
) -> Response {
    match spawn(runner, task).await {
        Ok(task) => send_task(id, task).await,
        Err(reason) => {
            warn!(agent = runner.agent_name(), %reason, "Could not start a worker");
            json_rpc_error(StatusCode::OK, id, INTERNAL_ERROR, reason)
        }
    }
}

/// Drain a task and answer with the finished A2A task object.
///
/// No timeout here on purpose: a task is over when the participant says so or
/// when its socket closes, and the socket close is guaranteed to arrive
/// because the listener deregisters on it. A caller that wants a deadline has
/// one — `A2aClient` sets a read timeout — and a second one here would cut
/// off long turns that are working fine.
///
/// # A question ends it (AGE-321)
///
/// Non-terminal progress has nowhere to go in a non-streaming reply;
/// `message/stream` is the method that carries it. `input-required` is the
/// one kind that cannot simply be dropped: the worker is parked on a question
/// this caller will never see, so waiting would buy nothing but the worker's
/// own clarification timeout — minutes of silence ending in a failure that
/// does not say a question was ever asked.
///
/// So the task ends here, and the failure quotes the question. The caller
/// learns what was wanted and can ask again over `message/stream`, which can
/// carry both the question and the answer. Holding the task open for
/// `tasks/get` polling instead would make non-streaming callers first-class,
/// and it is the A2A-shaped answer, but it makes the broker stateful for open
/// tasks; that was weighed and deliberately not taken here.
async fn send_task(id: Option<Value>, mut task: RunningTask) -> Response {
    let mut text = String::new();
    let mut state = TaskState::Failed;
    let mut message = Some("the participant ended the task without a final status".to_string());
    let mut metadata = None;

    while let Some(update) = task.updates.recv().await {
        match update {
            TaskUpdate::Artifact { text: chunk, .. } => text.push_str(&chunk),
            TaskUpdate::Status {
                state: s,
                message: m,
                metadata: d,
                ..
            } => {
                if s == TaskState::InputRequired {
                    state = TaskState::Failed;
                    message = Some(unanswerable_question(m.as_deref(), d.as_ref()));
                    metadata = d;
                    break;
                }
                if s.is_terminal() {
                    state = s;
                    message = m;
                    metadata = d;
                }
            }
        }
    }
    // Dropping the task cancels it, which closes the worker's socket and
    // un-parks its `ask_user` — the question dies with the task rather than
    // waiting out a timeout nobody is going to beat.
    task.finish(state == TaskState::Completed, metadata.as_ref());
    // Appended whether the task succeeded or failed: a failed worker's
    // partial edits are still on that branch (AGE-399).
    if let Some(hint) = task.worker.as_ref().and_then(|w| w.merge_hint()) {
        text.push_str(hint);
    }

    let mut result = json!({
        "id": task.task_id,
        "status": { "state": state.to_string() },
        "artifacts": [{ "parts": [{ "type": "text", "text": text }] }],
    });
    if let Some(message) = message {
        result["status"]["message"] = json!({ "parts": [{ "type": "text", "text": message }] });
    }
    if let Some(metadata) = metadata {
        result["status"]["metadata"] = metadata;
    }

    json_rpc_ok(id, result)
}

// ---------------------------------------------------------------------------
// message/stream
// ---------------------------------------------------------------------------

/// Stream a registered participant's task as A2A SSE events.
pub(crate) fn message_stream(
    registry: &ParticipantRegistry,
    name: &str,
    id: Option<Value>,
    task: DelegatedTask,
) -> Response {
    match submit(registry, name, task) {
        Some(task) => stream_task(id, task),
        None => failed_stream(
            id,
            "unknown",
            format!("participant '{name}' is no longer connected"),
        ),
    }
}

/// Start a worker and stream its task.
pub(crate) async fn runner_message_stream(
    runner: &dyn VirtualAgent,
    id: Option<Value>,
    task: DelegatedTask,
) -> Response {
    match spawn(runner, task).await {
        Ok(task) => stream_task(id, task),
        Err(reason) => {
            warn!(agent = runner.agent_name(), %reason, "Could not start a worker");
            failed_stream(id, "unknown", reason)
        }
    }
}

/// The frames are byte-identical in shape to the module path's, which is what
/// lets `A2aClient::send_message_stream` parse them with no branch on what is
/// behind the endpoint.
fn stream_task(id: Option<Value>, mut task: RunningTask) -> Response {
    let stream = async_stream::stream! {
        let task_id = task.task_id.clone();
        yield sse(&status_event(&id, &task_id, "working", None, None, false));

        let mut ended = false;
        while let Some(update) = task.updates.recv().await {
            match update {
                TaskUpdate::Artifact { text, last_chunk } => {
                    yield sse(&artifact_event(&id, &task_id, &text, last_chunk));
                }
                TaskUpdate::Status { state, message, metadata, input } => {
                    let terminal = state.is_terminal();
                    // Sent as an artifact chunk, ahead of the terminal status:
                    // `A2aClient` stops reading the instant it sees a `final`
                    // status, so anything after that point is never seen
                    // (AGE-399). Sent whether the task succeeded or failed —
                    // a failed worker's partial edits are still on that branch.
                    if terminal
                        && let Some(hint) = task.worker.as_ref().and_then(|w| w.merge_hint())
                    {
                        yield sse(&artifact_event(&id, &task_id, hint, true));
                    }
                    yield sse(&status_event(
                        &id,
                        &task_id,
                        &state.to_string(),
                        message.as_deref(),
                        with_clarification(metadata.clone(), input),
                        terminal,
                    ));
                    if terminal {
                        task.finish(state == TaskState::Completed, metadata.as_ref());
                        ended = true;
                        break;
                    }
                }
            }
        }

        // The stream can only end without a terminal status if the
        // participant's sender was dropped without one. `deregister` sends a
        // `failed` before dropping, so this is the belt to that braces — a
        // caller must never be left without a final event.
        if !ended {
            debug!(task = %task_id, "Participant task ended with no final status");
            yield sse(&status_event(
                &id,
                &task_id,
                "failed",
                Some("the participant ended the task without a final status"),
                None,
                true,
            ));
        }
        // `task` is dropped here, which reaps a spawned worker and runs its
        // workspace's `on_exit`.
        drop(task);
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Fold what a parked task is waiting for into the status's `metadata`, next
/// to whatever else rides there.
fn with_clarification(metadata: Option<Value>, input: Option<InputRequest>) -> Option<Value> {
    let Some(input) = input else {
        return metadata;
    };
    let mut metadata = match metadata {
        Some(Value::Object(map)) => Value::Object(map),
        // A non-object metadata cannot be extended; the request wins, since
        // without it the caller cannot answer.
        _ => json!({}),
    };
    metadata[CLARIFICATION_METADATA_KEY] = json!(input);
    Some(metadata)
}

// ---------------------------------------------------------------------------
// message/send on a parked task
// ---------------------------------------------------------------------------

/// Answer a task parked in `input-required`.
///
/// A2A's way of resuming a task is `message/send` with the message's
/// `taskId` set; the answers ride in the message's `metadata` under
/// [`CLARIFICATION_METADATA_KEY`] as a [`TaskInput`]. The reply is the task
/// as it stands — `working` — rather than the finished task: the caller is
/// already consuming the task's stream, and a second reader of one task's
/// updates would need the registry to fan out, which nothing else needs.
///
/// A `message/send` whose `taskId` the broker does not hold open starts a
/// new task instead; only [`ParticipantRegistry::owns_task`] tells the two
/// apart, and the router asks it before coming here.
pub(crate) fn message_input(
    registry: &ParticipantRegistry,
    id: Option<Value>,
    task_id: &str,
    params: &Value,
) -> Response {
    let input = params
        .pointer("/message/metadata")
        .and_then(|m| m.get(CLARIFICATION_METADATA_KEY))
        .cloned()
        .and_then(|v| serde_json::from_value::<TaskInput>(v).ok());
    let Some(input) = input else {
        return json_rpc_error(
            StatusCode::OK,
            id,
            INVALID_PARAMS,
            format!(
                "task '{task_id}' is waiting for input; the message must carry \
                 `metadata.{CLARIFICATION_METADATA_KEY}` with `requestId` and `answers`"
            ),
        );
    };

    match registry.answer_task(task_id, input) {
        Ok(()) => json_rpc_ok(
            id,
            json!({
                "id": task_id,
                "status": { "state": TaskState::Working.to_string() },
            }),
        ),
        Err(e) => {
            warn!(task = %task_id, error = %e, "Could not deliver an answer");
            json_rpc_error(StatusCode::OK, id, INTERNAL_ERROR, e.to_string())
        }
    }
}

/// A single-event SSE response for a failure that happened before the task
/// existed. Same shape as a mid-stream failure so callers need no special
/// case.
fn failed_stream(id: Option<Value>, task_id: &str, reason: String) -> Response {
    let event = status_event(&id, task_id, "failed", Some(&reason), None, true);
    let stream = async_stream::stream! { yield sse(&event); };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn artifact_event(id: &Option<Value>, task_id: &str, text: &str, last_chunk: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "id": task_id,
            "artifact": {
                "parts": [{ "type": "text", "text": text }],
                "index": 0,
                "lastChunk": last_chunk,
            }
        }
    })
}

fn status_event(
    id: &Option<Value>,
    task_id: &str,
    state: &str,
    message: Option<&str>,
    metadata: Option<Value>,
    is_final: bool,
) -> Value {
    let mut status = json!({ "state": state });
    if let Some(message) = message {
        status["message"] = json!({ "parts": [{ "type": "text", "text": message }] });
    }
    if let Some(metadata) = metadata {
        status["metadata"] = metadata;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "id": task_id,
            "status": status,
            "final": is_final,
        }
    })
}

fn sse(value: &Value) -> Result<Event, std::convert::Infallible> {
    Ok(Event::default().data(value.to_string()))
}

// ---------------------------------------------------------------------------
// Caller liveness
// ---------------------------------------------------------------------------

/// Cancels the task if the caller goes away before it finishes.
///
/// An HTTP client that hangs up mid-stream drops the response body, which
/// drops the generator, which drops this. Without it the participant would
/// keep working — and keep holding a slot in the concurrency budget
/// (AGE-305) — for a caller that stopped listening.
struct TaskGuard {
    registry: ParticipantRegistry,
    participant: String,
    task_id: String,
    done: bool,
}

impl TaskGuard {
    fn new(registry: ParticipantRegistry, participant: String, task_id: String) -> Self {
        Self {
            registry,
            participant,
            task_id,
            done: false,
        }
    }

    fn finished(&mut self) {
        self.done = true;
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.done {
            debug!(
                participant = %self.participant,
                task = %self.task_id,
                "Caller hung up; cancelling the task"
            );
            self.registry.cancel_task(&self.participant, &self.task_id);
        }
    }
}
