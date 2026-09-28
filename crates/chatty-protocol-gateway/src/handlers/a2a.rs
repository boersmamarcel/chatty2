//! A2A (Agent-to-Agent) protocol handlers.
//!
//! Routes:
//! - `GET  /a2a/{module}/.well-known/agent.json` — per-module agent card
//! - `POST /a2a/{module}` — A2A JSON-RPC (`message/send`, `message/stream`,
//!   `tasks/get`); a `message/send` whose `message.taskId` names a task
//!   parked in `input-required` answers it instead of starting a new one
//! - `GET  /.well-known/agent.json` — aggregated gateway agent card

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use chatty_wasm_runtime::{AgentCard, ChatRequest, Message, Role};
use serde_json::{Value, json};

use crate::gateway::GatewayState;
use crate::participant::{CALLER_HEADER, DelegatedTask, TaskBearer};
use chatty_fabric::AgentOrigin;

use super::a2a_participant;
use super::jsonrpc::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND,
    json_rpc_error, json_rpc_ok, module_not_found, module_not_found_json,
};
use super::module_call::{self, Protocol};
use super::openai::should_route_remotely;

// ---------------------------------------------------------------------------
// Handler: GET /a2a/{module}/.well-known/agent.json
// ---------------------------------------------------------------------------

pub(crate) async fn module_agent_card(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    if let Some(card) = state.participants.card(&module_name) {
        return (StatusCode::OK, Json(a2a_participant::card_to_json(&card))).into_response();
    }

    if let Some(runner) = state.runners.get(&module_name) {
        return (
            StatusCode::OK,
            Json(a2a_participant::card_to_json(&runner.agent_card())),
        )
            .into_response();
    }

    let Some(module) = module_call::module_for(&state, &module_name, Protocol::A2a).await else {
        return module_not_found_json(&module_name);
    };

    match module_call::blocking(module, |m| m.agent_card()).await {
        Ok(card) => (StatusCode::OK, Json(agent_card_to_json(&card))).into_response(),
        Err(e) => (
            module_call::failure_status(&e),
            Json(json!({ "error": format!("{e:#}") })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Handler: GET /.well-known/agent.json  (aggregated)
// ---------------------------------------------------------------------------

pub(crate) async fn aggregated_agent_card(State(state): State<GatewayState>) -> impl IntoResponse {
    // Only the modules served over A2A are agents of this gateway.
    let modules: Vec<_> = {
        let reg = state.registry.read().await;
        reg.module_names()
            .filter(|name| reg.manifest(name).is_some_and(|m| m.protocols.a2a))
            .filter_map(|name| reg.get(name))
            .collect()
    };

    let mut agents: Vec<Value> = Vec::new();

    // Every agent on this card says where it came from (ADR-0011 C5): a
    // caller cannot tell a child process from a third-party URL by name
    // alone, and the broker is the only thing that knows.
    for module in modules {
        if let Ok(card) = module_call::blocking(module, |m| m.agent_card()).await {
            agents.push(with_origin(agent_card_to_json(&card), AgentOrigin::Local));
        }
    }

    // Registered processes are agents of this gateway too (ADR-0011); a
    // caller reading the aggregated card should see everything it can
    // address, not only what happens to be a WASM module.
    for agent in state.participants.agents() {
        agents.push(with_origin(
            a2a_participant::card_to_json(&agent.card),
            agent.origin,
        ));
    }

    // A runner has no process until a task arrives, but it is the agent a
    // caller addresses to get one, so every one of them belongs on the card
    // (ADR-0011 C10). What it spawns is a child of this machine.
    for runner in state.runners.values() {
        agents.push(with_origin(
            a2a_participant::card_to_json(&runner.agent_card()),
            AgentOrigin::Local,
        ));
    }

    Json(json!({
        "schema_version": "0.1",
        "gateway": true,
        "agents": agents,
    }))
}

/// Tag one agent card with its origin.
fn with_origin(mut card: Value, origin: AgentOrigin) -> Value {
    if let Some(object) = card.as_object_mut() {
        object.insert("origin".to_string(), json!(origin.as_str()));
    }
    card
}

async fn forward_remote_a2a_jsonrpc(
    module_name: &str,
    body: Value,
    state: &GatewayState,
) -> Result<Response, String> {
    let runner_url = state
        .runner_url
        .as_ref()
        .ok_or_else(|| "Remote execution requested but runner URL is not configured".to_string())?;
    let url = format!("{}/a2a/{}", runner_url.trim_end_matches('/'), module_name);

    let client = reqwest::Client::new();
    let mut req_builder = client.post(&url).json(&body);
    if let Some(hive_client) = state.hive_client.as_ref() {
        if let Some(token) = hive_client.access_token().await {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        } else {
            tracing::warn!(
                module = module_name,
                "Remote A2A forwarding: hive_client has no token — runner will reject with 401"
            );
        }
    }

    let response = req_builder
        .send()
        .await
        .map_err(|e| format!("Failed to connect to runner: {}", e))?;

    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if content_type.contains("text/event-stream") {
        let stream = async_stream::stream! {
            let mut response = response;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => yield Ok::<_, std::io::Error>(chunk),
                    Ok(None) => break,
                    Err(err) => {
                        yield Err(std::io::Error::other(format!("runner SSE read failed: {}", err)));
                        break;
                    }
                }
            }
        };
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from_stream(stream))
            .map_err(|e| format!("Failed to build proxied SSE response: {}", e));
    }

    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read runner response: {}", e))?;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body_bytes))
        .map_err(|e| format!("Failed to build proxied response: {}", e))
}

// ---------------------------------------------------------------------------
// Handler: POST /a2a/{module}
// ---------------------------------------------------------------------------

pub(crate) async fn a2a_jsonrpc(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
    headers: HeaderMap,
    Json(body): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if body.jsonrpc != "2.0" {
        return json_rpc_error(
            StatusCode::BAD_REQUEST,
            body.id,
            INVALID_REQUEST,
            "Invalid Request: jsonrpc must be \"2.0\"",
        );
    }

    // The caller's bearer goes with the task to whichever worker runs it
    // (AGE-371): the broker validates nothing here — a hosted worker checks
    // it as the tenant boundary, a local one ignores it.
    let bearer = caller_bearer(&headers);
    // Which broker worker is asking, if one is (AGE-628).
    let caller = headers
        .get(CALLER_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    match body.method.as_str() {
        "message/send" => {
            handle_message_send(&module_name, body.id, body.params, bearer, caller, &state).await
        }
        "message/stream" => {
            handle_message_stream(&module_name, body.id, body.params, bearer, caller, &state)
                .await
                .into_response()
        }
        "tasks/get" => handle_tasks_get(&module_name, body.id, body.params).await,
        method => json_rpc_error(
            StatusCode::OK,
            body.id,
            METHOD_NOT_FOUND,
            format!("Method not found: {}", method),
        ),
    }
}

// ---------------------------------------------------------------------------
// message/send: forward to module's chat export
// ---------------------------------------------------------------------------

async fn handle_message_send(
    module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
    bearer: Option<TaskBearer>,
    caller: Option<String>,
    state: &GatewayState,
) -> axum::response::Response {
    let params = match params {
        Some(p) => p,
        None => {
            return json_rpc_error(
                StatusCode::OK,
                id,
                INVALID_PARAMS,
                "params are required for message/send",
            );
        }
    };

    // A message addressed to a task the broker holds open is the answer to
    // a question that task asked (AGE-306), not a new task. Only an id the
    // broker minted itself can match, so a client that puts its own id on a
    // fresh message still starts a task.
    if let Some(task_id) = params
        .pointer("/message/taskId")
        .and_then(|v| v.as_str())
        .filter(|task_id| state.participants.owns_task(task_id))
    {
        tracing::info!(task = task_id, "A2A: answering a parked task");
        return a2a_participant::message_input(&state.participants, id, task_id, &params);
    }

    let content = prompt_text(&params);

    // A registered process shadows a module of the same name: it is live,
    // and a module is not.
    if state.participants.is_registered(module_name) {
        tracing::info!(
            participant = module_name,
            "A2A: routing to a local participant"
        );
        let task = DelegatedTask::new(content).with_bearer(bearer);
        return a2a_participant::message_send(&state.participants, module_name, id, task).await;
    }

    if let Some(runner) = state.runners.get(module_name) {
        tracing::info!(agent = module_name, "A2A: starting a worker");
        let task = DelegatedTask::new(content)
            .with_bearer(bearer)
            .with_caller(caller);
        return a2a_participant::runner_message_send(runner.as_ref(), id, task).await;
    }

    // Remote routing: if the registry says this module is `remote`/`remote_only`,
    // forward to the hive-runner's A2A endpoint so the remote execution keeps
    // the same JSON-RPC shape as local module execution.
    if should_route_remotely(module_name, state).await {
        tracing::info!(module = module_name, "A2A: routing to remote runner");
        return match forward_remote_a2a_jsonrpc(
            module_name,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "message/send",
                "params": params,
            }),
            state,
        )
        .await
        {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(module = module_name, error = %e, "Remote A2A execution failed");
                json_rpc_error(
                    StatusCode::OK,
                    id,
                    INTERNAL_ERROR,
                    format!("Remote execution failed: {}", e),
                )
            }
        };
    }

    let Some(module) = module_call::module_for(state, module_name, Protocol::A2a).await else {
        return module_not_found(id, module_name);
    };

    // Pre-invocation credit check
    if let Err(e) = module_call::check_credits(state, module_name).await {
        return json_rpc_error(StatusCode::OK, id, -32000, e);
    }
    if let Err(e) = module_call::check_usage_reporting(state, module_name) {
        return json_rpc_error(StatusCode::OK, id, -32000, e);
    }

    let turn = Turn::new(module_name, &params, content, &state.contexts);
    let mut module = module.lock().await;
    let result = module.chat(turn.request()).await;
    let metrics = module.last_invocation_metrics();
    drop(module);

    match result {
        Ok(resp) => {
            module_call::record_usage(state, module_name, metrics);
            let context_id = turn.context_id.clone();
            turn.record(&resp.content);

            json_rpc_ok(
                id,
                json!({
                    "id": format!("task-{}", crate::gateway::new_id()),
                    "contextId": context_id,
                    "status": { "state": "completed" },
                    "artifacts": [{
                        "parts": [{ "type": "text", "text": resp.content }]
                    }]
                }),
            )
        }
        Err(e) => json_rpc_error(
            module_call::failure_status(&e),
            id,
            INTERNAL_ERROR,
            format!("{e:#}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// message/stream: SSE streaming variant of message/send
// ---------------------------------------------------------------------------

async fn handle_message_stream(
    module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
    bearer: Option<TaskBearer>,
    caller: Option<String>,
    state: &GatewayState,
) -> axum::response::Response {
    let params = match params {
        Some(p) => p,
        None => {
            return json_rpc_error(
                StatusCode::OK,
                id,
                INVALID_PARAMS,
                "params are required for message/stream",
            );
        }
    };

    let content = prompt_text(&params);

    if state.participants.is_registered(module_name) {
        tracing::info!(
            participant = module_name,
            "A2A stream: routing to a local participant"
        );
        let task = DelegatedTask::new(content).with_bearer(bearer);
        return a2a_participant::message_stream(&state.participants, module_name, id, task);
    }

    if let Some(runner) = state.runners.get(module_name) {
        tracing::info!(agent = module_name, "A2A stream: starting a worker");
        let task = DelegatedTask::new(content)
            .with_bearer(bearer)
            .with_caller(caller);
        return a2a_participant::runner_message_stream(runner.as_ref(), id, task).await;
    }

    let task_id = format!("task-{}", crate::gateway::new_id());
    let module_name = module_name.to_string();

    // Remote routing check must come BEFORE registry lookup — remote modules
    // are not loaded into the local WASM registry (no binary), so checking the
    // registry first would return 404 for them.
    if should_route_remotely(&module_name, state).await {
        tracing::info!(module = %module_name, "A2A stream: routing to remote runner");
        return match forward_remote_a2a_jsonrpc(
            &module_name,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "message/stream",
                "params": params,
            }),
            state,
        )
        .await
        {
            Ok(response) => response,
            Err(e) => {
                let stream = async_stream::stream! {
                    let failed = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "id": task_id,
                            "status": {
                                "state": "failed",
                                "message": { "parts": [{ "type": "text", "text": format!("Remote execution failed: {}", e) }] }
                            },
                            "final": true
                        }
                    });
                    yield Ok::<_, std::convert::Infallible>(Event::default().data(failed.to_string()));
                };
                Sse::new(stream)
                    .keep_alive(KeepAlive::default())
                    .into_response()
            }
        };
    }

    let Some(module) = module_call::module_for(state, &module_name, Protocol::A2a).await else {
        return module_not_found(id, &module_name);
    };
    if let Err(e) = module_call::check_credits(state, &module_name).await {
        return json_rpc_error(StatusCode::OK, id, -32000, e);
    }
    if let Err(e) = module_call::check_usage_reporting(state, &module_name) {
        return json_rpc_error(StatusCode::OK, id, -32000, e);
    }

    let turn = Turn::new(&module_name, &params, content, &state.contexts);
    let context_id = turn.context_id.clone();

    // Module log lines, forwarded as `working` progress while the call runs.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // The call runs in a task of its own, so a caller that disconnects does
    // not cancel it: it runs to its end (bounded by the per-call deadline),
    // records its turn, and releases the module (S3 row 3.9).
    let mut chat_handle = tokio::spawn({
        let state = state.clone();
        let module_name = module_name.clone();
        async move {
            let mut module = module.lock_owned().await;
            module.set_progress_sender(progress_tx);
            let result = module.chat(turn.request()).await;
            let metrics = module.last_invocation_metrics();
            drop(module);
            if let Ok(resp) = &result {
                module_call::record_usage(&state, &module_name, metrics);
                turn.record(&resp.content);
            }
            result
        }
    });

    let event = move |result: Value| {
        let mut result = result;
        if let Some(object) = result.as_object_mut() {
            object.insert("id".into(), json!(task_id));
            object.insert("contextId".into(), json!(context_id));
        }
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        Ok::<_, std::convert::Infallible>(Event::default().data(frame.to_string()))
    };
    let working = |message: Option<&str>| match message {
        None => json!({ "status": { "state": "working" }, "final": false }),
        Some(text) => json!({
            "status": {
                "state": "working",
                "message": { "parts": [{ "type": "text", "text": text }] }
            },
            "final": false
        }),
    };
    let failed = |text: String| {
        json!({
            "status": {
                "state": "failed",
                "message": { "parts": [{ "type": "text", "text": text }] }
            },
            "final": true
        })
    };

    let stream = async_stream::stream! {
        yield event(working(None));

        // Progress first (biased), until the call returns.
        let joined = loop {
            tokio::select! {
                biased;
                Some(line) = progress_rx.recv() => yield event(working(Some(&line))),
                joined = &mut chat_handle => break joined,
            }
        };
        while let Ok(line) = progress_rx.try_recv() {
            yield event(working(Some(&line)));
        }

        match joined {
            Ok(Ok(resp)) => {
                yield event(json!({
                    "artifact": {
                        "parts": [{ "type": "text", "text": resp.content }],
                        "index": 0,
                        "lastChunk": true
                    }
                }));
                yield event(json!({ "status": { "state": "completed" }, "final": true }));
            }
            Ok(Err(e)) => yield event(failed(format!("{e:#}"))),
            Err(e) => yield event(failed(format!("Task panicked: {e}"))),
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ---------------------------------------------------------------------------
// Conversation history per A2A context
// ---------------------------------------------------------------------------

/// How many contexts the gateway remembers; the oldest is forgotten first.
const MAX_CONTEXTS: usize = 1024;
/// How many messages one context keeps; the oldest are dropped first.
const MAX_CONTEXT_MESSAGES: usize = 256;

/// A2A conversation history, per module and `contextId`: what a module has
/// been told and answered in a context, replayed in front of the context's
/// next message. Bounded in contexts and in messages per context.
#[derive(Clone, Default)]
pub(crate) struct Contexts(Arc<Mutex<ContextMap>>);

#[derive(Default)]
struct ContextMap {
    turns: HashMap<(String, String), Vec<Message>>,
    /// Keys in first-seen order, for eviction.
    order: VecDeque<(String, String)>,
}

impl Contexts {
    fn history(&self, module: &str, context_id: &str) -> Vec<Message> {
        self.lock()
            .turns
            .get(&(module.to_string(), context_id.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    fn record(&self, module: &str, context_id: &str, user: Message, reply: &str) {
        let key = (module.to_string(), context_id.to_string());
        let mut map = self.lock();
        if !map.turns.contains_key(&key) {
            map.order.push_back(key.clone());
            while map.order.len() > MAX_CONTEXTS {
                if let Some(oldest) = map.order.pop_front() {
                    map.turns.remove(&oldest);
                }
            }
        }
        let messages = map.turns.entry(key).or_default();
        messages.push(user);
        messages.push(Message {
            role: Role::Assistant,
            content: reply.to_string(),
        });
        let excess = messages.len().saturating_sub(MAX_CONTEXT_MESSAGES);
        messages.drain(..excess);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ContextMap> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One A2A turn against a module: the context it belongs to, the new user
/// message, and the history in front of it.
struct Turn {
    module: String,
    context_id: String,
    user: Message,
    history: Vec<Message>,
    contexts: Contexts,
}

impl Turn {
    /// The turn for `params`. A message without a `contextId` starts a new
    /// context, whose id the answer carries so the caller can continue it.
    fn new(module: &str, params: &Value, content: String, contexts: &Contexts) -> Self {
        let context_id = params
            .pointer("/message/contextId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(crate::gateway::new_id);
        Self {
            module: module.to_string(),
            history: contexts.history(module, &context_id),
            context_id,
            user: Message {
                role: Role::User,
                content,
            },
            contexts: contexts.clone(),
        }
    }

    /// The guest's request: the context's history, then this message.
    fn request(&self) -> ChatRequest {
        let mut messages = self.history.clone();
        messages.push(self.user.clone());
        ChatRequest {
            messages,
            conversation_id: self.context_id.clone(),
        }
    }

    /// Remember this turn and the module's reply in the context.
    fn record(self, reply: &str) {
        self.contexts
            .record(&self.module, &self.context_id, self.user, reply);
    }
}

// ---------------------------------------------------------------------------
// tasks/get: return a simple "not found" since we are stateless
// ---------------------------------------------------------------------------

async fn handle_tasks_get(
    _module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
) -> axum::response::Response {
    let task_id = params
        .as_ref()
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Stateless gateway — we don't persist tasks across requests.
    json_rpc_error(
        StatusCode::OK,
        id,
        INVALID_PARAMS,
        format!("task '{}' not found (stateless gateway)", task_id),
    )
}

// ---------------------------------------------------------------------------
// Helper: the caller's `Authorization: Bearer` token, if any (AGE-371)
// ---------------------------------------------------------------------------

/// What the A2A client put in `Authorization`, as a task bearer. Anything
/// that is not a bearer scheme is treated as no token: the worker that gets
/// the task decides what an absent bearer means, not this router.
fn caller_bearer(headers: &HeaderMap) -> Option<TaskBearer> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
        .map(TaskBearer::new)
}

// ---------------------------------------------------------------------------
// Helper: the prompt out of A2A `message/send` / `message/stream` params
// ---------------------------------------------------------------------------

/// Every text part of `message.parts`, joined by newlines (a part is text
/// when its `kind` or `type` says so, or it has only a `text`); or a plain
/// `message.text`, which several clients send.
fn prompt_text(params: &Value) -> String {
    if let Some(parts) = params.pointer("/message/parts").and_then(Value::as_array) {
        let texts: Vec<&str> = parts
            .iter()
            .filter(|part| {
                let kind = part.get("kind").or_else(|| part.get("type"));
                kind.is_none_or(|kind| kind == "text")
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    params
        .pointer("/message/text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Helper: serialize an AgentCard to JSON
// ---------------------------------------------------------------------------

pub(crate) fn agent_card_to_json(card: &AgentCard) -> Value {
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
        "displayName": card.display_name,
        "description": card.description,
        "version": card.version,
        "skills": skills,
        "capabilities": {
            "streaming": true
        },
    })
}
