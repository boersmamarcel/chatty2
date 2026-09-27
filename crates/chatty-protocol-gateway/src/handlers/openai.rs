//! OpenAI-compatible chat completion handlers.
//!
//! Routes:
//! - `POST /v1/{module}/chat/completions` — per-module OpenAI chat completion
//! - `POST /v1/chat/completions` — model-routed via `model: "module:{name}"`

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chatty_wasm_runtime::{ChatRequest, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::gateway::GatewayState;

use super::module_call::{self, Protocol};

// ---------------------------------------------------------------------------
// OpenAI request / response shapes
// ---------------------------------------------------------------------------

/// OpenAI chat completion request body.
///
/// `temperature` and `max_tokens` are accepted for API compatibility but not
/// forwarded: the WASM module owns its own model configuration. `stream:
/// true` is refused with a 400 (PL-D1 option B: a module's OpenAI route is
/// exposure only, and non-streaming). `user` becomes the guest's
/// `conversation_id`.
#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<OaiMessage>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// One request message. `content` is a string, an array of content parts
/// (whose `text` parts are joined), or null.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OaiMessage {
    pub role: String,
    #[serde(default)]
    pub content: Value,
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

/// The request's messages as the guest's, every role kept. The WIT has
/// `system`, `user` and `assistant`; `developer` is OpenAI's newer name for
/// `system`. Any other role (`tool`, `function`) has no guest equivalent and
/// is refused rather than passed off as something it is not.
fn convert_messages(oai_messages: &[OaiMessage]) -> Result<Vec<Message>, String> {
    oai_messages
        .iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "system" | "developer" => Role::System,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                other => return Err(format!("unsupported message role '{other}'")),
            };
            Ok(Message {
                role,
                content: content_text(&m.content)?,
            })
        })
        .collect()
}

/// The text of a message's `content`: the string itself, or every `text`
/// part of a content-part array joined by newlines.
fn content_text(content: &Value) -> Result<String, String> {
    match content {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => Ok(parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")),
        _ => Err("message content must be a string or an array of parts".to_string()),
    }
}

/// An OpenAI-shaped error body with `status`.
fn openai_error(
    status: StatusCode,
    kind: &str,
    message: impl Into<String>,
) -> axum::response::Response {
    (
        status,
        Json(json!({ "error": { "message": message.into(), "type": kind } })),
    )
        .into_response()
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
            return openai_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "model must be in format 'module:{name}' for this endpoint",
            );
        }
    };

    run_chat(&module_name, &body, state).await
}

// ---------------------------------------------------------------------------
// Shared chat runner
// ---------------------------------------------------------------------------

/// Execute a chat completion request remotely via hive-runner.
pub(crate) async fn execute_remotely(
    module_name: &str,
    body: &ChatCompletionRequest,
    state: &GatewayState,
) -> Result<ChatCompletionResponse, String> {
    let runner_url = state
        .runner_url
        .as_ref()
        .ok_or_else(|| "Remote execution requested but runner_url not configured".to_string())?;

    let url = format!(
        "{}/v1/{}/chat/completions",
        runner_url.trim_end_matches('/'),
        module_name
    );

    // Forward the user's Hive Bearer token so the runner can authenticate the
    // request, look up the user, and deduct credits from the correct account
    // (A4 for #72).
    let client = reqwest::Client::new();
    let mut req_builder = client.post(&url).json(&body);
    if let Some(hive_client) = state.hive_client.as_ref() {
        if let Some(token) = hive_client.access_token().await {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        } else {
            tracing::warn!(
                "Remote execution: hive_client has no token — runner will reject with 401"
            );
        }
    }

    let response = req_builder
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
    if body.stream == Some(true) {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "streaming not supported: a module's OpenAI route answers `stream: false` only",
        );
    }

    // Check if we should route to remote execution
    let should_execute_remotely = should_route_remotely(module_name, &state).await;

    // If remote execution is required, delegate to runner
    if should_execute_remotely {
        if state.runner_url.is_none() {
            return openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_error",
                format!(
                    "Module '{}' requires remote execution but runner URL is not configured",
                    module_name
                ),
            );
        }

        return match execute_remotely(module_name, body, &state).await {
            Ok(response) => (
                StatusCode::OK,
                Json(serde_json::to_value(response).unwrap_or_default()),
            )
                .into_response(),
            Err(e) => openai_error(
                StatusCode::BAD_GATEWAY,
                "remote_execution_error",
                format!("Remote execution failed: {}", e),
            ),
        };
    }

    // Otherwise, proceed with local execution
    let messages = match convert_messages(&body.messages) {
        Ok(messages) => messages,
        Err(e) => return openai_error(StatusCode::BAD_REQUEST, "invalid_request_error", e),
    };
    let req = ChatRequest {
        messages,
        conversation_id: body.user.clone().unwrap_or_default(),
    };

    let Some(module) = module_call::module_for(&state, module_name, Protocol::OpenAi).await else {
        return openai_error(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            format!("module '{}' not found", module_name),
        );
    };

    // Pre-invocation credit check for paid modules only
    if let Err(e) = module_call::check_credits(&state, module_name).await {
        return openai_error(StatusCode::PAYMENT_REQUIRED, "insufficient_credits", e);
    }

    let mut module = module.lock().await;
    let result = module.chat(req).await;
    let metrics = module.last_invocation_metrics();
    drop(module);

    match result {
        Ok(resp) => {
            module_call::record_usage(&state, module_name, metrics);

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
        Err(e) => openai_error(
            module_call::failure_status(&e),
            "server_error",
            format!("{e:#}"),
        ),
    }
}

/// Returns true when the module's registry metadata says it should run on
/// the remote runner (`execution_mode` ∈ {`remote`, `remote_only`}).
/// Falls back to local on errors / when no hive_client is configured.
pub(crate) async fn should_route_remotely(module_name: &str, state: &GatewayState) -> bool {
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

// ---------------------------------------------------------------------------
// Index entry builder (used by index handler)
// ---------------------------------------------------------------------------
