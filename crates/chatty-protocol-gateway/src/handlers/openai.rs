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
use tokio::sync::RwLock;

use chatty_module_registry::ModuleRegistry;

// ---------------------------------------------------------------------------
// OpenAI request / response shapes
// ---------------------------------------------------------------------------

/// OpenAI chat completion request body.
///
/// The `temperature`, `max_tokens`, and `stream` fields are accepted for
/// API compatibility but are not forwarded to the WASM module, which owns
/// its own model configuration.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub(crate) struct OaiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: UsageStats,
}

#[derive(Debug, Serialize)]
pub(crate) struct Choice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AssistantMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
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
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
    Json(body): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    run_chat(&module_name, &body, registry).await
}

// ---------------------------------------------------------------------------
// Handler: POST /v1/chat/completions  (model-routed)
// ---------------------------------------------------------------------------

pub(crate) async fn chat_completions_routed(
    State(registry): State<Arc<RwLock<ModuleRegistry>>>,
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

    run_chat(&module_name, &body, registry).await
}

// ---------------------------------------------------------------------------
// Shared chat runner
// ---------------------------------------------------------------------------

async fn run_chat(
    module_name: &str,
    body: &ChatCompletionRequest,
    registry: Arc<RwLock<ModuleRegistry>>,
) -> axum::response::Response {
    let messages = convert_messages(&body.messages);
    let req = ChatRequest {
        messages,
        conversation_id: String::new(),
    };

    let mut reg = registry.write().await;
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

    match module.chat(req).await {
        Ok(resp) => {
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
