//! OpenAI-compatible chat completion handlers.
//!
//! Routes:
//! - `POST /v1/{module}/chat/completions` — per-module OpenAI chat completion
//! - `POST /v1/chat/completions` — model-routed via `model: "module:{name}"`

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chatty_wasm_runtime::{ChatRequest, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::gateway::GatewayState;

// ---------------------------------------------------------------------------
// OpenAI request / response shapes
// ---------------------------------------------------------------------------

/// OpenAI chat completion request body.
///
/// The `temperature`, `max_tokens`, and `stream` fields are accepted for
/// API compatibility but are not forwarded to the WASM module, which owns
/// its own model configuration.
#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<OaiMessage>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OaiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: UsageStats,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Choice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AssistantMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UsageStats {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn convert_messages(oai_messages: &[OaiMessage]) -> Vec<Message> {
    oai_messages
        .iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "assistant" => Role::Assistant,
                _ => Role::User,
            };
            Message {
                role,
                content: m.content.clone(),
            }
        })
        .collect()
}

fn build_response(
    model: &str,
    content: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> ChatCompletionResponse {
    use std::time::{SystemTime, UNIX_EPOCH};
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    ChatCompletionResponse {
        id: format!("chatcmpl-{}", crate::gateway::new_id()),
        object: "chat.completion".to_string(),
        created,
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: AssistantMessage {
                role: "assistant".to_string(),
                content: content.to_string(),
            },
            finish_reason: "stop".to_string(),
        }],
        usage: UsageStats {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    }
}

// ---------------------------------------------------------------------------
// Handler: POST /v1/{module}/chat/completions
// ---------------------------------------------------------------------------

pub(crate) async fn chat_completions_module(
    Path(module_name): Path<String>,
    State(state): State<GatewayState>,
    Json(body): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    run_chat(&module_name, &body, state).await
}

// ---------------------------------------------------------------------------
// Handler: POST /v1/chat/completions  (model-routed)
// ---------------------------------------------------------------------------

pub(crate) async fn chat_completions_routed(
    State(state): State<GatewayState>,
    Json(body): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    // Expect model in format "module:{name}"
    let module_name = match body.model.strip_prefix("module:") {
        Some(name) => name.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "model must be in format 'module:{name}' for this endpoint",
                        "type": "invalid_request_error",
                    }
                })),
            )
                .into_response();
        }
    };

    run_chat(&module_name, &body, state).await
}

// ---------------------------------------------------------------------------
// Shared chat runner
// ---------------------------------------------------------------------------

/// Execute a chat completion request remotely via hive-runner.
async fn execute_remotely(
    module_name: &str,
    body: &ChatCompletionRequest,
    state: &GatewayState,
) -> Result<ChatCompletionResponse, String> {
    let runner_url = state
        .runner_url
        .as_ref()
        .ok_or_else(|| "Remote execution requested but runner_url not configured".to_string())?;

    let url = format!("{}/v1/{}/chat/completions", runner_url.trim_end_matches('/'), module_name);

    // Build the request - forward the original body
    let client = reqwest::Client::new();

    // Add auth token if available from hive_client
    if let Some(ref _hive_client) = state.hive_client {
        // Extract token from the client using reflection through debug or by storing it separately
        // For now, we'll need to pass the token explicitly - this is a known limitation
        // The token should be stored in the gateway state or extracted from hive_client
        tracing::warn!("Remote execution: auth token not yet passed to runner (requires Phase 4 enhancement)");
    }

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to connect to runner: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Runner returned error {}: {}", status, body));
    }

    response
        .json::<ChatCompletionResponse>()
        .await
        .map_err(|e| format!("Failed to parse runner response: {}", e))
}

async fn run_chat(
    module_name: &str,
    body: &ChatCompletionRequest,
    state: GatewayState,
) -> axum::response::Response {
    // Check if we should route to remote execution
    // First, check if module metadata indicates remote execution
    let should_execute_remotely = if let Some(ref hive_client) = state.hive_client {
        match hive_client.get_module(module_name).await {
            Ok(metadata) => {
                let exec_mode = metadata.execution_mode.as_str();
                tracing::debug!(module = module_name, execution_mode = exec_mode, "Checked module execution mode");
                
                match exec_mode {
                    "remote_only" => true,
                    "remote" => {
                        // For now, keep "remote" as local by default
                        // This could be made configurable with a preference flag in the future
                        tracing::debug!(module = module_name, "Module prefers remote execution but using local by default");
                        false
                    }
                    _ => false, // "local" or unknown
                }
            }
            Err(e) => {
                // If we can't fetch metadata, assume local execution
                tracing::warn!(module = module_name, error = %e, "Failed to fetch module metadata, assuming local execution");
                false
            }
        }
    } else {
        false
    };

    // If remote execution is required, delegate to runner
    if should_execute_remotely {
        if state.runner_url.is_none() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": {
                        "message": format!("Module '{}' requires remote execution but runner URL is not configured", module_name),
                        "type": "configuration_error",
                    }
                })),
            )
                .into_response();
        }

        return match execute_remotely(module_name, body, &state).await {
            Ok(response) => (StatusCode::OK, Json(serde_json::to_value(response).unwrap_or_default())).into_response(),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("Remote execution failed: {}", e),
                        "type": "remote_execution_error",
                    }
                })),
            )
                .into_response(),
        };
    }

    // Otherwise, proceed with local execution
    let messages = convert_messages(&body.messages);
    let req = ChatRequest {
        messages,
        conversation_id: String::new(),
    };

    let mut reg = state.registry.write().await;
    let module = match reg.get_mut(module_name) {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("module '{}' not found", module_name),
                        "type": "invalid_request_error",
                    }
                })),
            )
                .into_response();
        }
    };

    // Pre-invocation credit check for paid modules only
    if let Some(ref guard) = state.credit_guard {
        if state.paid_modules.contains(module_name) {
            if let Err(e) = guard.has_credits(module_name).await {
                drop(reg);
                return (
                    StatusCode::PAYMENT_REQUIRED,
                    Json(json!({
                        "error": {
                            "message": e.to_string(),
                            "type": "insufficient_credits",
                        }
                    })),
                )
                    .into_response();
            }
        }
    }

    match module.chat(req).await {
        Ok(resp) => {
            let metrics = module.last_invocation_metrics();
            drop(reg);

            if let Some(ref usage) = state.usage {
                tokio::spawn({
                    let usage = Arc::clone(usage);
                    let name = module_name.to_string();
                    async move {
                        usage.record_invocation(
                            &name,
                            "latest",
                            metrics.as_ref().and_then(|m| m.input_tokens.map(|t| t as i32)),
                            metrics.as_ref().and_then(|m| m.output_tokens.map(|t| t as i32)),
                            metrics.as_ref().map(|m| m.fuel_consumed),
                            metrics.as_ref().map(|m| m.execution_ms),
                        ).await;
                    }
                });
            }

            let (prompt_tokens, completion_tokens) = resp
                .usage
                .map(|u| (u.input_tokens, u.output_tokens))
                .unwrap_or((0, 0));
            let model = format!("module:{}", module_name);
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(build_response(
                        &model,
                        &resp.content,
                        prompt_tokens,
                        completion_tokens,
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": e.to_string(),
                    "type": "server_error",
                }
            })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Index entry builder (used by index handler)
// ---------------------------------------------------------------------------
