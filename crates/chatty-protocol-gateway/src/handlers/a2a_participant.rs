//! Serving a registered local participant over the gateway's A2A surface.
//!
//! A participant's task updates ([`TaskUpdate`]) become the same JSON-RPC
//! and SSE frames a WASM module's do, so `A2aClient` — and anything else
//! speaking A2A — cannot tell the two apart. That equivalence is the point:
//! ADR-0011's first kill criterion asks whether a delegated turn survives
//! the round trip at the granularity the parent already renders, and it can
//! only be answered if the wire shape is the one the parent already reads.

use axum::{
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde_json::{Value, json};
use tracing::debug;

use crate::participant::{ParticipantCard, ParticipantRegistry, TaskState, TaskStream, TaskUpdate};

use super::jsonrpc::{INTERNAL_ERROR, json_rpc_error, json_rpc_ok};

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
// message/send
// ---------------------------------------------------------------------------

/// Run one task to completion and answer with the finished A2A task.
///
/// No timeout here on purpose: a task is over when the participant says so
/// or when its socket closes, and the socket close is guaranteed to arrive
/// because the listener deregisters on it. A caller that wants a deadline
/// has one — `A2aClient` sets an HTTP timeout — and enforcing a second one
/// here would cut off long turns that are working fine.
pub(crate) async fn message_send(
    registry: &ParticipantRegistry,
    name: &str,
    id: Option<Value>,
    prompt: String,
) -> Response {
    let Some((task_id, updates)) = registry.submit_task(name, prompt) else {
        return json_rpc_error(
            StatusCode::OK,
            id,
            INTERNAL_ERROR,
            format!("participant '{name}' is no longer connected"),
        );
    };

    let guard = TaskGuard::new(registry.clone(), name.to_string(), task_id.clone());
    let outcome = collect(updates).await;
    guard.finished();

    let mut result = json!({
        "id": task_id,
        "status": { "state": outcome.state.to_string() },
        "artifacts": [{
            "parts": [{ "type": "text", "text": outcome.text }]
        }],
    });
    if let Some(message) = outcome.message {
        result["status"]["message"] = json!({ "parts": [{ "type": "text", "text": message }] });
    }

    json_rpc_ok(id, result)
}

struct Outcome {
    state: TaskState,
    message: Option<String>,
    text: String,
}

/// Drain a task's updates into one finished result.
async fn collect(mut updates: TaskStream) -> Outcome {
    let mut text = String::new();
    let mut state = TaskState::Failed;
    let mut message = Some("the participant ended the task without a final status".to_string());

    while let Some(update) = updates.recv().await {
        match update {
            TaskUpdate::Artifact { text: chunk, .. } => text.push_str(&chunk),
            TaskUpdate::Status {
                state: s,
                message: m,
            } => {
                if s.is_terminal() {
                    state = s;
                    message = m;
                }
                // Non-terminal progress has nowhere to go in a non-streaming
                // reply; `message/stream` is the method that carries it.
            }
        }
    }

    Outcome {
        state,
        message,
        text,
    }
}

// ---------------------------------------------------------------------------
// message/stream
// ---------------------------------------------------------------------------

/// Stream a task's updates as A2A SSE events.
///
/// The frames are byte-identical in shape to the module path's, which is
/// what lets `A2aClient::send_message_stream` parse them with no branch on
/// what is behind the endpoint.
pub(crate) fn message_stream(
    registry: &ParticipantRegistry,
    name: &str,
    id: Option<Value>,
    prompt: String,
) -> Response {
    let Some((task_id, mut updates)) = registry.submit_task(name, prompt) else {
        return failed_stream(
            id,
            "unknown",
            format!("participant '{name}' is no longer connected"),
        );
    };

    let guard = TaskGuard::new(registry.clone(), name.to_string(), task_id.clone());

    let stream = async_stream::stream! {
        yield sse(&status_event(&id, &task_id, "working", None, false));

        let mut ended = false;
        while let Some(update) = updates.recv().await {
            match update {
                TaskUpdate::Artifact { text, last_chunk } => {
                    yield sse(&json!({
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
                    }));
                }
                TaskUpdate::Status { state, message } => {
                    let terminal = state.is_terminal();
                    yield sse(&status_event(
                        &id,
                        &task_id,
                        &state.to_string(),
                        message.as_deref(),
                        terminal,
                    ));
                    if terminal {
                        ended = true;
                        break;
                    }
                }
            }
        }

        // The stream can only end without a terminal status if the
        // participant's sender was dropped without one. `deregister` sends a
        // `failed` before dropping, so this is the belt to that braces —
        // a caller must never be left without a final event.
        if !ended {
            debug!(task = %task_id, "Participant task ended with no final status");
            yield sse(&status_event(
                &id,
                &task_id,
                "failed",
                Some("the participant ended the task without a final status"),
                true,
            ));
        }
        guard.finished();
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// A single-event SSE response for a failure that happened before the task
/// existed. Same shape as a mid-stream failure so callers need no special
/// case.
fn failed_stream(id: Option<Value>, task_id: &str, reason: String) -> Response {
    let event = status_event(&id, task_id, "failed", Some(&reason), true);
    let stream = async_stream::stream! { yield sse(&event); };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn status_event(
    id: &Option<Value>,
    task_id: &str,
    state: &str,
    message: Option<&str>,
    is_final: bool,
) -> Value {
    let mut status = json!({ "state": state });
    if let Some(message) = message {
        status["message"] = json!({ "parts": [{ "type": "text", "text": message }] });
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

    /// The task reached a terminal state on its own; nothing to cancel.
    fn finished(mut self) {
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
