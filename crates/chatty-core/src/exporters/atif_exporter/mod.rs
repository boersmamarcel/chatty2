//! ATIF (Agent Trace Interchange Format) exporter.
//!
//! Converts a chatty `Conversation` (with full message history, tool calls,
//! and feedback) into the ATIF JSON schema for sharing or training.
//!
//! # What lives here
//!
//! - Top-level `conversation_to_atif` and helpers that map each message,
//!   tool call, attachment, and feedback record into the ATIF type system.
//! - Schema versioning, metadata stamping, and tool-call ordering rules.
//!
//! # What does NOT live here
//!
//! - The ATIF type definitions themselves — `exporters::types`.
//! - Other export formats — sibling files in `exporters/` (markdown, PDF,
//!   JSONL for SFT/DPO).
//! - Persistence — the caller writes the returned JSON to disk.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rig::completion::Message;
use rig::completion::message::{AssistantContent, UserContent};

use crate::exporters::types::*;
use crate::models::conversation::{MessageFeedback, RegenerationRecord};
use crate::models::message_types::{SystemTrace, TraceItem};
use crate::models::token_usage::ConversationTokenUsage;
use crate::repositories::ConversationData;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderType;

/// ATIF schema version this exporter produces.
const SCHEMA_VERSION: &str = "ATIF-v1.6";

/// Convert a persisted conversation into ATIF JSON format.
///
/// This is a pure function with no side effects — it takes data and returns
/// a JSON value without touching global state or performing I/O.
///
/// Phases:
/// 1. Deserialize all double-serialized JSON strings in ConversationData
/// 2. Build the agent block from model_id and optional ModelConfig
/// 3. Iterate through message history, building one ATIF step per message
/// 4. Build final_metrics from ConversationTokenUsage totals
/// 5. Build the extra block (feedback + regenerations)
pub fn conversation_to_atif(
    conversation: &ConversationData,
    model_config: Option<&ModelConfig>,
) -> Result<serde_json::Value> {
    // PHASE 1: Deserialize all parallel arrays
    let history: Vec<Message> = serde_json::from_str(&conversation.message_history)
        .context("Failed to parse message_history")?;
    let traces: Vec<Option<serde_json::Value>> = serde_json::from_str(&conversation.system_traces)
        .context("Failed to parse system_traces")?;
    let token_usage: ConversationTokenUsage =
        serde_json::from_str(&conversation.token_usage).unwrap_or_default();
    let attachment_paths: Vec<Vec<String>> =
        serde_json::from_str(&conversation.attachment_paths).unwrap_or_default();
    let timestamps: Vec<Option<i64>> =
        serde_json::from_str(&conversation.message_timestamps).unwrap_or_default();
    let feedback: Vec<Option<MessageFeedback>> =
        serde_json::from_str(&conversation.message_feedback).unwrap_or_default();
    let regeneration_records: Vec<RegenerationRecord> =
        serde_json::from_str(&conversation.regeneration_records).unwrap_or_default();

    // PHASE 2: Build agent block
    let agent = build_agent(&conversation.model_id, model_config);

    // PHASE 3: Build steps (track assistant turn index for token_usage lookup)
    let mut steps = Vec::with_capacity(history.len());
    let mut assistant_turn_idx: usize = 0;

    for (idx, message) in history.iter().enumerate() {
        let timestamp = timestamps.get(idx).copied().flatten();
        let msg_attachments = attachment_paths.get(idx).cloned().unwrap_or_default();
        let trace_json = traces.get(idx).cloned().flatten();
        let step_id = (idx as u32) + 1;

        let step =
            match message {
                Message::User { content } => {
                    build_user_step(step_id, content, timestamp, &msg_attachments)
                }
                Message::Assistant { content, .. } => {
                    let metrics = token_usage.message_usages.get(assistant_turn_idx).map(|u| {
                        AtifStepMetrics {
                            prompt_tokens: Some(u.input_tokens),
                            completion_tokens: Some(u.output_tokens),
                            cost_usd: u.estimated_cost_usd,
                        }
                    });
                    assistant_turn_idx += 1;
                    build_agent_step(step_id, content, timestamp, trace_json, metrics)
                }
                Message::System { .. } => continue,
            };
        steps.push(step);
    }

    // PHASE 4: Build final_metrics
    let final_metrics = AtifFinalMetrics {
        total_prompt_tokens: Some(token_usage.total_input_tokens),
        total_completion_tokens: Some(token_usage.total_output_tokens),
        total_cost_usd: Some(token_usage.total_estimated_cost_usd),
        total_steps: Some(steps.len() as u32),
    };

    // PHASE 5: Build extra (feedback + regenerations)
    let extra = build_extra(&feedback, &regeneration_records);

    let export = AtifExport {
        schema_version: SCHEMA_VERSION.to_string(),
        session_id: conversation.id.clone(),
        agent,
        steps,
        final_metrics: Some(final_metrics),
        extra: Some(extra),
    };

    serde_json::to_value(&export).context("Failed to serialize ATIF export")
}


mod steps;
use steps::*;


#[cfg(test)]
mod tests;
