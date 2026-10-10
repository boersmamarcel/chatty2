//! ATIF (Agent Trace Interchange Format) exporter.
//!
//! Converts a chatty `Conversation` (with full message history, tool calls,
//! and feedback) into the ATIF JSON schema for sharing or training.
//!
//! # What lives here
//!
//! - `run_to_atif` (behind `exporters::export_run`): one document for a
//!   whole run, the root conversation's steps with every agent it delegated
//!   to nested under the delegation step that started it (AGE-859).
//! - Helpers that map each message, tool call, attachment, and feedback
//!   record into the ATIF type system; schema versioning.
//! - `export_swarm`: the same document from a live swarm trace, every step
//!   attributed to its agent and every plugin tool call to its plugin
//!   (TB-2), and `swarm_tree_from_atif`, which reads one back.
//!
//! # What does NOT live here
//!
//! - The ATIF type definitions themselves — `exporters::types`.
//! - Other export formats — sibling files in `exporters/` (markdown, PDF,
//!   JSONL for SFT/DPO).
//! - Persistence — the caller writes the returned JSON to disk.

use anyhow::{Context, Result};
use rig_core::completion::Message;
use rig_core::completion::message::UserContent;

use crate::exporters::types::*;
use crate::models::conversation::{MessageFeedback, RegenerationRecord};
use crate::models::token_usage::{ConversationTokenUsage, TokenUsage};
use crate::repositories::ConversationData;
use crate::services::swarm_trace::{AGENTS_TRACE_KEY, AgentRecord, UsageLine};
use crate::settings::models::models_store::ModelConfig;

/// ATIF schema version this exporter produces.
const SCHEMA_VERSION: &str = "ATIF-v1.6";

/// The root conversation of a run, read for [`run_to_atif`]: its steps,
/// one per message, and the agents each turn delegated to.
struct RootConversation {
    agent: AtifAgent,
    steps: Vec<Built>,
    /// Each turn that delegated: the index of its step in `steps` and the
    /// agents its trace kept (AGE-859).
    turns: Vec<(usize, Vec<AgentRecord>)>,
    /// The root's own usage lines: its turns and its plugins'.
    own_usage: Vec<UsageLine>,
    assistant_turns: u32,
    token_usage: ConversationTokenUsage,
    extra: AtifExtra,
}

/// Read a persisted conversation into its steps.
///
/// Phases:
/// 1. Deserialize all double-serialized JSON strings in ConversationData
/// 2. Build the agent block from model_id and optional ModelConfig
/// 3. Iterate through message history, building one ATIF step per message,
///    and collect the agents each turn's trace kept
/// 4. Build the extra block (feedback + regenerations)
fn root_conversation(
    conversation: &ConversationData,
    model_config: Option<&ModelConfig>,
) -> Result<RootConversation> {
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

    // PHASE 3: Build steps (track assistant turn index for token_usage lookup).
    // A delegated worker's usage is its own line on the record (AGE-415),
    // not an assistant turn: it is the worker's steps' (AGE-859), so only
    // the turns' own usage is aligned here.
    let own_usages: Vec<&TokenUsage> = token_usage
        .message_usages
        .iter()
        .filter(|u| u.delegated_to.is_none())
        .collect();
    let own_usage = own_usages
        .iter()
        .flat_map(|u| UsageLine::from_token_usage(u))
        .collect();
    let mut steps = Vec::with_capacity(history.len());
    let mut turns = Vec::new();
    let mut assistant_turn_idx: usize = 0;

    for (idx, message) in history.iter().enumerate() {
        // Tool round-trips are persisted in history (AGE-247): the model
        // turn that asked for the calls is a step of its own, and each
        // result joins the step whose call it answers (AGE-863). Their usage
        // is the turn's, on its final text message.
        let round_trip = crate::services::is_persisted_tool_round_trip(&history, idx);
        if round_trip && let Message::User { content } = message {
            for part in content.iter() {
                if let UserContent::ToolResult(result) = part {
                    attach_tool_result(&mut steps, result);
                }
            }
            continue;
        }
        let timestamp = timestamps.get(idx).copied().flatten();
        let msg_attachments = attachment_paths.get(idx).cloned().unwrap_or_default();
        let trace_json = traces.get(idx).cloned().flatten();
        let records: Vec<AgentRecord> = trace_json
            .as_ref()
            .and_then(|trace| trace.get(AGENTS_TRACE_KEY))
            .and_then(|records| serde_json::from_value(records.clone()).ok())
            .unwrap_or_default();
        let step_id = (idx as u32) + 1;

        let built = match message {
            Message::User { content } => Built::new(
                build_user_step(step_id, content, timestamp, &msg_attachments),
                Vec::new(),
            ),
            Message::Assistant { content, .. } if round_trip => Built::new(
                build_agent_step(step_id, content, timestamp, None, None),
                call_refs(content),
            ),
            Message::Assistant { content, .. } => {
                let metrics = own_usages.get(assistant_turn_idx).map(|u| AtifStepMetrics {
                    prompt_tokens: Some(u.prompt_tokens()),
                    completion_tokens: Some(u.output_tokens),
                    cost_usd: u.estimated_cost_usd,
                });
                assistant_turn_idx += 1;
                Built::new(
                    build_agent_step(step_id, content, timestamp, trace_json, metrics),
                    call_refs(content),
                )
            }
            Message::System { .. } => continue,
        };
        if records.len() > 1 {
            turns.push((steps.len(), records));
        }
        steps.push(built);
    }

    // PHASE 4: Build extra (feedback + regenerations)
    let extra = build_extra(&feedback, &regeneration_records);

    Ok(RootConversation {
        agent,
        steps,
        turns,
        own_usage,
        assistant_turns: assistant_turn_idx as u32,
        token_usage,
        extra,
    })
}

mod steps;
use steps::*;
mod swarm;
use swarm::{Built, attach_tool_result, call_refs};
pub use swarm::{export_swarm, run_to_atif, swarm_to_atif, swarm_tree_from_atif};

#[cfg(test)]
mod tests;
