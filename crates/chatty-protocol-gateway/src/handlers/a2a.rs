//! A2A (Agent-to-Agent) protocol handlers.
//!
//! Routes:
//! - `GET  /a2a/{module}/.well-known/agent.json` — per-module agent card
//! - `POST /a2a/{module}` — A2A JSON-RPC (`message/send`, `message/stream`,
//!   `tasks/get`)
//! - `GET  /.well-known/agent.json` — aggregated gateway agent card

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use chatty_module_registry::ModuleRegistry;
use chatty_wasm_runtime::AgentCard;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::jsonrpc::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, JsonRpcRequest,
    json_rpc_error, json_rpc_ok, module_not_found, module_not_found_json,
};

// ---------------------------------------------------------------------------
// Handler: GET /a2a/{module}/.well-known/agent.json
// ---------------------------------------------------------------------------

pub(crate) async fn module_agent_card(
    Path(module_name): Path<String>,
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
) -> impl IntoResponse {
    let mut reg = registry.write().await;
    let module = match reg.get_mut(&module_name) {
        Some(m) => m,
        None => {
            return module_not_found_json(&module_name);
        }
    };

    match module.agent_card() {
        Ok(card) => (StatusCode::OK, Json(agent_card_to_json(&card))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Handler: GET /.well-known/agent.json  (aggregated)
// ---------------------------------------------------------------------------

pub(crate) async fn aggregated_agent_card(
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
) -> impl IntoResponse {
    let names: Vec<String> = {
        let reg = registry.read().await;
        reg.module_names().map(str::to_string).collect()
    };

    let mut agents: Vec<Value> = Vec::new();

    for name in &names {
        let mut reg = registry.write().await;
        if let Some(module) = reg.get_mut(name)
            && let Ok(card) = module.agent_card()
        {
            agents.push(agent_card_to_json(&card));
        }
    }

    Json(json!({
        "schema_version": "0.1",
        "gateway": true,
        "agents": agents,
    }))
}

// ---------------------------------------------------------------------------
// Handler: POST /a2a/{module}
// ---------------------------------------------------------------------------

pub(crate) async fn a2a_jsonrpc(
    Path(module_name): Path<String>,
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
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

    match body.method.as_str() {
        "message/send" => handle_message_send(&module_name, body.id, body.params, registry).await,
        "message/stream" => handle_message_stream(&module_name, body.id, body.params, registry)
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
// message/send: forward to module's chat export
// ---------------------------------------------------------------------------

async fn handle_message_send(
    module_name: &str,
    id: Option<Value>,
    params: Option<Value>,
    registry: Arc<RwLock<ModuleRegistry>>,
) -> axum::response::Response {
    use chatty_wasm_runtime::{ChatRequest, Message, Role};

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

    // Extract text from params: support both `message.parts[0].text` and plain `message.text`
    let content = params
        .pointer("/message/parts/0/text")
        .or_else(|| params.pointer("/message/text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let req = ChatRequest {
        messages: vec![Message {
            role: Role::User,
            content,
        }],
        conversation_id: String::new(),
    };

    let mut reg = registry.write().await;
    let module = match reg.get_mut(module_name) {
        Some(m) => m,
        None => {
            return module_not_found(id, module_name);
        }
    };

    match module.chat(req).await {
        Ok(resp) => {
            let task_id = format!("task-{}", crate::gateway::new_id());
            json_rpc_ok(
                id,
                json!({
                    "id": task_id,
                    "status": { "state": "completed" },
                    "artifacts": [{
                        "parts": [{ "type": "text", "text": resp.content }]
                    }]
                }),
            )
        }
        Err(e) => json_rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            id,
            INTERNAL_ERROR,
            e.to_string(),
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
    registry: Arc<RwLock<ModuleRegistry>>,
) -> axum::response::Response {
    use chatty_wasm_runtime::{ChatRequest, Message, Role};

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

    let content = params
        .pointer("/message/parts/0/text")
        .or_else(|| params.pointer("/message/text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let task_id = format!("task-{}", crate::gateway::new_id());
    let module_name = module_name.to_string();

    // Verify the module exists before starting the stream.
    {
        let reg = registry.read().await;
        if reg.get(&module_name).is_none() {
            return module_not_found(id.clone(), &module_name);
        }
    }

    let req = ChatRequest {
        messages: vec![Message {
            role: Role::User,
            content,
        }],
        conversation_id: String::new(),
    };

    // Create a progress channel for real-time streaming of module log messages.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Build an SSE stream that interleaves progress events with the chat result.
    let stream = async_stream::stream! {
        // 1. "working" status
        let working = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "id": task_id,
                "status": { "state": "working" },
                "final": false
            }
        });
        yield Ok::<_, std::convert::Infallible>(Event::default().data(working.to_string()));

        // 2. Spawn chat in a separate task, install progress sender
        let mut chat_handle = tokio::task::spawn_blocking({
            let registry = registry.clone();
            let module_name = module_name.clone();
            let req = req;
            let progress_tx = progress_tx;
            move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async {
                    let mut reg = registry.write().await;
                    if let Some(m) = reg.get_mut(&module_name) {
                        m.set_progress_sender(progress_tx);
                        m.chat(req).await
                    } else {
                        Err(anyhow::anyhow!("module '{}' not found", module_name))
                    }
                })
            }
        });

        // 3. Interleave progress events with chat completion
        let mut chat_done = false;
        let mut chat_result = None;

        loop {
            if chat_done {
                break;
            }

            tokio::select! {
                biased;
                // Check for progress messages first
                Some(msg) = progress_rx.recv() => {
                    let progress = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "id": task_id,
                            "status": {
                                "state": "working",
                                "message": { "parts": [{ "type": "text", "text": msg }] }
                            },
                            "final": false
                        }
                    });
                    yield Ok(Event::default().data(progress.to_string()));
                }
                // Chat task completed
                result = &mut chat_handle => {
                    chat_done = true;
                    chat_result = Some(result);
                }
            }
        }

        // Drain any remaining progress
        while let Ok(msg) = progress_rx.try_recv() {
            let progress = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "id": task_id,
                    "status": {
                        "state": "working",
                        "message": { "parts": [{ "type": "text", "text": msg }] }
                    },
                    "final": false
                }
            });
            yield Ok(Event::default().data(progress.to_string()));
        }

        // 4. Emit final result
        match chat_result {
            Some(Ok(Ok(resp))) => {
                let artifact = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "id": task_id,
                        "artifact": {
                            "parts": [{ "type": "text", "text": resp.content }],
                            "index": 0,
                            "lastChunk": true
                        }
                    }
                });
                yield Ok(Event::default().data(artifact.to_string()));

                let completed = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "id": task_id,
                        "status": { "state": "completed" },
                        "final": true
                    }
                });
                yield Ok(Event::default().data(completed.to_string()));
            }
            Some(Ok(Err(e))) => {
                let failed = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "id": task_id,
                        "status": {
                            "state": "failed",
                            "message": { "parts": [{ "type": "text", "text": e.to_string() }] }
                        },
                        "final": true
                    }
                });
                yield Ok(Event::default().data(failed.to_string()));
            }
            Some(Err(e)) => {
                let failed = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "id": task_id,
                        "status": {
                            "state": "failed",
                            "message": { "parts": [{ "type": "text", "text": format!("Task panicked: {e}") }] }
                        },
                        "final": true
                    }
                });
                yield Ok(Event::default().data(failed.to_string()));
            }
            None => {
                let failed = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "id": task_id,
                        "status": {
                            "state": "failed",
                            "message": { "parts": [{ "type": "text", "text": "No result received" }] }
                        },
                        "final": true
                    }
                });
                yield Ok(Event::default().data(failed.to_string()));
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
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
