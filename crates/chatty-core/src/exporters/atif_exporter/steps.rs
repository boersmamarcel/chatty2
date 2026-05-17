//! ATIF step builders — pure helpers used by `conversation_to_atif`.
//!
//! # What lives here
//!
//! - `build_agent` / `provider_name` / `format_timestamp` / `image_media_type`
//!   — small lookup helpers.
//! - `build_user_step` / `build_agent_step` — one ATIF step per chat message.
//! - `parse_trace` / `build_extra` — feedback + regeneration metadata.
//!
//! All functions are pure (no I/O, no globals). Re-exported as `use steps::*`
//! from `mod.rs` to preserve the original call-site form unchanged.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rig::completion::Message;
use rig::completion::message::{AssistantContent, UserContent};

use crate::exporters::types::*;
use crate::models::conversation::{MessageFeedback, RegenerationRecord};
use crate::models::message_types::{SystemTrace, TraceItem};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderType;

pub(super) fn build_agent(model_id: &str, model_config: Option<&ModelConfig>) -> AtifAgent {
    match model_config {
        Some(cfg) => {
            let provider = provider_name(&cfg.provider_type);
            AtifAgent {
                name: "chatty".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                model_name: Some(cfg.model_identifier.clone()),
                extra: Some(serde_json::json!({ "provider": provider })),
            }
        }
        None => AtifAgent {
            name: "chatty".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            model_name: Some(model_id.to_string()),
            extra: Some(serde_json::json!({ "provider": "unknown" })),
        },
    }
}

pub(super) fn provider_name(provider_type: &ProviderType) -> String {
    match provider_type {
        ProviderType::OpenRouter => "openrouter",
        ProviderType::Ollama => "ollama",
        ProviderType::AzureOpenAI => "azure_openai",
    }
    .to_string()
}

/// Format a unix epoch timestamp as ISO 8601 UTC string.
pub(super) fn format_timestamp(epoch_secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch_secs, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| format!("{}", epoch_secs))
}

/// Classify a file path as an image MIME type, or return None for non-image files.
pub(super) fn image_media_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
    {
        Some(ext) => match ext.as_str() {
            "jpg" | "jpeg" => Some("image/jpeg"),
            "png" => Some("image/png"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            _ => None,
        },
        None => None,
    }
}

pub(super) fn build_user_step(
    step_id: u32,
    content: &rig::OneOrMany<UserContent>,
    timestamp: Option<i64>,
    attachment_paths: &[String],
) -> AtifStep {
    let text: String = content
        .iter()
        .filter_map(|uc| match uc {
            UserContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Build message: plain string if no image attachments, ContentPart array otherwise
    let image_attachments: Vec<&String> = attachment_paths
        .iter()
        .filter(|p| image_media_type(Path::new(p)).is_some())
        .collect();

    let message = if image_attachments.is_empty() {
        AtifMessage::Text(text)
    } else {
        let mut parts = vec![AtifContentPart::Text { text }];
        for path in image_attachments {
            let media_type = image_media_type(Path::new(path)).unwrap_or("image/png");
            parts.push(AtifContentPart::Image {
                source: AtifImageSource {
                    media_type: media_type.to_string(),
                    path: path.clone(),
                },
            });
        }
        AtifMessage::Parts(parts)
    };

    AtifStep {
        step_id,
        timestamp: timestamp.map(format_timestamp),
        source: "user".to_string(),
        message,
        reasoning_content: None,
        tool_calls: None,
        observation: None,
        metrics: None,
    }
}

pub(super) fn build_agent_step(
    step_id: u32,
    content: &rig::OneOrMany<AssistantContent>,
    timestamp: Option<i64>,
    trace_json: Option<serde_json::Value>,
    metrics: Option<AtifStepMetrics>,
) -> AtifStep {
    let text: String = content
        .iter()
        .filter_map(|ac| match ac {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    // Parse system trace for reasoning content and tool outputs (keyed by ToolCallBlock.id)
    let (reasoning_content, trace_outputs) = parse_trace(trace_json);

    // Build tool calls from rig AssistantContent, collecting observation results
    let mut tool_calls_vec: Vec<AtifToolCall> = Vec::new();
    let mut observation_results: Vec<AtifObservationResult> = Vec::new();

    for ac in content.iter() {
        if let AssistantContent::ToolCall(tc) = ac {
            let atif_id = tc.call_id.clone().unwrap_or_else(|| tc.id.clone());

            tool_calls_vec.push(AtifToolCall {
                tool_call_id: atif_id.clone(),
                function_name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            });

            // If we have output from the trace, add it to observation results
            if let Some(output) = trace_outputs.get(&tc.id) {
                observation_results.push(AtifObservationResult {
                    source_call_id: Some(atif_id),
                    content: Some(output.clone()),
                });
            }
        }
    }

    let tool_calls = if tool_calls_vec.is_empty() {
        None
    } else {
        Some(tool_calls_vec)
    };

    let observation = if observation_results.is_empty() {
        None
    } else {
        Some(AtifObservation {
            results: observation_results,
        })
    };

    AtifStep {
        step_id,
        timestamp: timestamp.map(format_timestamp),
        source: "agent".to_string(),
        message: AtifMessage::Text(text),
        reasoning_content,
        tool_calls,
        observation,
        metrics,
    }
}

/// Parse a system trace JSON, returning (reasoning_text, tool_output_by_id).
pub(super) fn parse_trace(trace_json: Option<serde_json::Value>) -> (Option<String>, HashMap<String, String>) {
    let mut outputs = HashMap::new();
    let trace_json = match trace_json {
        Some(v) => v,
        None => return (None, outputs),
    };

    let trace: SystemTrace = match serde_json::from_value(trace_json) {
        Ok(t) => t,
        Err(_) => return (None, outputs),
    };

    let reasoning_parts: Vec<String> = trace
        .items
        .iter()
        .filter_map(|item| match item {
            TraceItem::Thinking(tb) if !tb.content.is_empty() => Some(tb.content.clone()),
            _ => None,
        })
        .collect();

    let reasoning_content = if reasoning_parts.is_empty() {
        None
    } else {
        Some(reasoning_parts.join("\n\n"))
    };

    for item in &trace.items {
        if let TraceItem::ToolCall(tc) = item
            && let Some(output) = &tc.output
        {
            outputs.insert(tc.id.clone(), output.clone());
        }
    }

    (reasoning_content, outputs)
}

pub(super) fn build_extra(
    feedback: &[Option<MessageFeedback>],
    regenerations: &[RegenerationRecord],
) -> AtifExtra {
    let feedback_strings: Vec<Option<String>> = feedback
        .iter()
        .map(|f| match f {
            Some(MessageFeedback::ThumbsUp) => Some("thumbs_up".to_string()),
            Some(MessageFeedback::ThumbsDown) => Some("thumbs_down".to_string()),
            None => None,
        })
        .collect();

    let atif_regenerations: Vec<AtifRegeneration> = regenerations
        .iter()
        .map(|r| AtifRegeneration {
            message_index: r.message_index,
            original_text: r.original_text.clone(),
            timestamp: r.regeneration_timestamp,
        })
        .collect();

    AtifExtra {
        feedback: feedback_strings,
        regenerations: atif_regenerations,
    }
}

