//! A2A (Agent-to-Agent) protocol handlers.
//!
//! The agents served here are local participants and virtual agents (the
//! broker's workers). A WASM plugin is never an agent (`chatty:plugin@0.4.0`
//! has no `chat` export, PL-U3), so no module is served over A2A; a module
//! whose registry metadata says `remote` is still forwarded to the Hive
//! runner until PL-H8b removes that path.
//!
//! Routes:
//! - `GET  /a2a/{agent}/.well-known/agent.json` — per-agent card
//! - `POST /a2a/{agent}` — A2A JSON-RPC (`message/send`, `message/stream`,
//!   `tasks/get`)
//! - `GET  /.well-known/agent.json` — aggregated gateway agent card
//!
//! **Scope (BI-7, ADR-0020).** A worker calls `invoke_agent`/`list_agents`
//! over the connection the broker made for it (BI-4), never over this HTTP
//! side, so from here on `{agent}` naming a role (a registered participant
//! or a virtual agent) is refused with 403: identity comes from the
//! connection, and nothing on this machine may task a role, reach a handle
//! or read the swarm directory over loopback instead. A module name and
//! `forward_remote_a2a_jsonrpc` (PL-H8b) are unaffected — this route is the
//! only way a remote module is reached.

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde_json::{Value, json};

use crate::gateway::GatewayState;
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
        return refuse_role(&state, &module_name);
    }

    module_not_found_json(&module_name)
}

// ---------------------------------------------------------------------------
// Handler: GET /.well-known/agent.json  (aggregated)
// ---------------------------------------------------------------------------

pub(crate) async fn aggregated_agent_card(State(state): State<GatewayState>) -> impl IntoResponse {
    state.routes.count_directory();

    // Nothing is on this gateway's roster (no participant socket, no
    // virtual agent published): there is no swarm directory to protect, so
    // a bare `ProtocolGateway` — a module-only gateway, or a test that never
    // registers anything — keeps answering as it always has. The moment a
    // role exists, reading it over loopback is exactly what BI-7 refuses.
    if has_any_role(&state) {
        return refuse(&state, "directory");
    }

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
    .into_response()
}

/// Whether `name` is one of the broker's roles — a connected participant or
/// a virtual agent — rather than a module.
fn is_role(state: &GatewayState, name: &str) -> bool {
    state.participants.is_registered(name) || state.runners.contains_key(name)
}

/// Whether this gateway has any role at all: a connected participant, or a
/// published virtual agent. The swarm directory has nothing to disclose
/// when this is false (BI-7).
fn has_any_role(state: &GatewayState) -> bool {
    !state.runners.is_empty() || !state.participants.agents().is_empty()
}

/// A loopback request named `what` (a role, a node, a handle or the
/// directory): refused with 403, counted, and logged as a refusal (BI-7).
/// Roles are reached over the worker's connection now (ADR-0020, BI-4), and
/// there is no `loopback_roles` option to bring the old path back.
fn refuse(state: &GatewayState, what: &str) -> Response {
    state.calls.log_loopback_refusal(what);
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "fabric: roles are reached over the worker connection" })),
    )
        .into_response()
}

/// [`refuse`] for a named role, counting it on [`crate::gateway::RouteCounter`]
/// exactly as a served role request always has.
fn refuse_role(state: &GatewayState, name: &str) -> Response {
    state.routes.count_role();
    refuse(state, name)
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
    Json(body): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if is_role(&state, &module_name) {
        return refuse_role(&state, &module_name);
    }
    if body.jsonrpc != "2.0" {
        return json_rpc_error(
            StatusCode::BAD_REQUEST,
            body.id,
            INVALID_REQUEST,
            "Invalid Request: jsonrpc must be \"2.0\"",
        );
    }

    // No role is served here any more (BI-7), so nothing on this route
    // carries a caller's bearer to a worker's task frame, or the deleted
    // `x-chatty-broker-caller` header: identity comes from a worker's own
    // connection, not from a claim on this route. A module and the remote
    // runner it may forward to (PL-H8b) never read either.
    match body.method.as_str() {
        "message/send" => handle_message_send(&module_name, body.id, body.params, &state).await,
        "message/stream" => handle_message_stream(&module_name, body.id, body.params, &state)
            .await
            .into_response(),
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
// message/send: a role's parked task, or the remote runner
// ---------------------------------------------------------------------------

async fn handle_message_send(
    module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
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
    // a question that task asked (AGE-306), not a new task — but a role's
    // parked task is answered over its connection now (BI-5), never over
    // loopback: `owns_task` is address-agnostic (any URL naming a task id
    // it owns would reach it), so this is refused exactly as addressing the
    // role directly is (BI-7), not routed.
    if let Some(task_id) = params
        .pointer("/message/taskId")
        .and_then(|v| v.as_str())
        .filter(|task_id| state.participants.owns_task(task_id))
    {
        return refuse(state, task_id);
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
