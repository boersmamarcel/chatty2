//! A2A (Agent-to-Agent) protocol handlers.
//!
//! The agents served here are local participants and virtual agents (the
//! broker's workers). A WASM plugin is never an agent (`chatty:plugin@0.3.0`
//! has no `chat` export, PL-U3), so no module is served over A2A; a module
//! whose registry metadata says `remote` is still forwarded to the Hive
//! runner until PL-H8b removes that path.
//!
//! Routes:
//! - `GET  /a2a/{agent}/.well-known/agent.json` — per-agent card
//! - `POST /a2a/{agent}` — A2A JSON-RPC (`message/send`, `message/stream`,
//!   `tasks/get`); a `message/send` whose `message.taskId` names a task
//!   parked in `input-required` answers it instead of starting a new one
//! - `GET  /.well-known/agent.json` — aggregated gateway agent card

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
use serde_json::{Value, json};

use crate::gateway::GatewayState;
use crate::participant::{CALLER_HEADER, DelegatedTask, TaskBearer};
use chatty_fabric::AgentOrigin;

use super::a2a_participant;
use super::jsonrpc::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND,
    json_rpc_error, module_not_found, module_not_found_json,
};

// ---------------------------------------------------------------------------
// Handler: GET /a2a/{agent}/.well-known/agent.json
// ---------------------------------------------------------------------------

pub(crate) async fn agent_card(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    if is_role(&state, &module_name) {
        state.routes.count_role();
    }
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

    module_not_found_json(&module_name)
}

// ---------------------------------------------------------------------------
// Handler: GET /.well-known/agent.json  (aggregated)
// ---------------------------------------------------------------------------

pub(crate) async fn aggregated_agent_card(State(state): State<GatewayState>) -> impl IntoResponse {
    state.routes.count_directory();
    let mut agents: Vec<Value> = Vec::new();

    // Every agent on this card says where it came from (ADR-0011 C5): a
    // caller cannot tell a child process from a third-party URL by name
    // alone, and the broker is the only thing that knows. Registered
    // processes are agents of this gateway (ADR-0011); a WASM plugin never
    // is (PL-U3).
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

/// Whether `name` is one of the broker's roles — a connected participant or
/// a virtual agent — rather than a module.
fn is_role(state: &GatewayState, name: &str) -> bool {
    state.participants.is_registered(name) || state.runners.contains_key(name)
}

/// Tag one agent card with its origin.
fn with_origin(mut card: Value, origin: AgentOrigin) -> Value {
    if let Some(object) = card.as_object_mut() {
        object.insert("origin".to_string(), json!(origin.as_str()));
    }
    card
}

/// Returns true when the module's registry metadata says it should run on
/// the remote runner (`execution_mode` ∈ {`remote`, `remote_only`}).
/// Falls back to local on errors / when no hive_client is configured.
async fn should_route_remotely(module_name: &str, state: &GatewayState) -> bool {
    let Some(ref hive_client) = state.hive_client else {
        return false;
    };
    match hive_client.get_module(module_name).await {
        Ok(metadata) => {
            let exec_mode = metadata.execution_mode.as_str();
            tracing::debug!(
                module = module_name,
                execution_mode = exec_mode,
                "Checked module execution mode"
            );
            matches!(exec_mode, "remote" | "remote_only")
        }
        Err(e) => {
            tracing::warn!(
                module = module_name,
                error = %e,
                "Failed to fetch module metadata, assuming local execution"
            );
            false
        }
    }
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
    if is_role(&state, &module_name) {
        state.routes.count_role();
    }
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
// message/send: route to a participant, a virtual agent or the remote runner
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
            .with_caller(caller)
            .with_spawn_context(Some(root_spawn_context(state, runner.as_ref())));
        return a2a_participant::runner_message_send(runner.as_ref(), id, task).await;
    }

    // Remote routing: if the registry says this module is `remote`/`remote_only`,
    // forward to the hive-runner's A2A endpoint (removed by PL-H8b).
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

    module_not_found(id, module_name)
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
            .with_caller(caller)
            .with_spawn_context(Some(root_spawn_context(state, runner.as_ref())));
        return a2a_participant::runner_message_stream(runner.as_ref(), id, task).await;
    }

    let task_id = format!("task-{}", crate::gateway::new_id());
    let module_name = module_name.to_string();

    // Remote modules are not loaded into the local WASM registry (no
    // binary); they are forwarded to the Hive runner (removed by PL-H8b).
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

    module_not_found(id, &module_name)
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
/// The context a worker started over HTTP is spawned with: the root's
/// (BI-5). Only the root reaches a role over loopback; a worker's calls
/// travel over its connection, where the broker knows who is calling.
fn root_spawn_context(
    state: &GatewayState,
    runner: &dyn crate::participant::VirtualAgent,
) -> chatty_fabric::SpawnContext {
    use crate::participant::spawn_context;
    spawn_context::derive(&spawn_context::root(&state.runners, runner), runner)
}

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
