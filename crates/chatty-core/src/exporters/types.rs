use serde::{Deserialize, Serialize};

use crate::models::token_usage::ModelRef;
use crate::services::swarm_trace::{NodeStatus, UsageLine};

/// Top-level ATIF (Agent Trajectory Interchange Format) export structure.
///
/// Spec: <https://github.com/laude-institute/harbor/blob/main/docs/rfcs/0001-trajectory-format.md>
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifExport {
    pub schema_version: String,
    pub session_id: String,
    pub agent: AtifAgent,
    pub steps: Vec<AtifStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<AtifFinalMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<AtifExtra>,
}

/// AgentSchema — identifies the agent system, not just the LLM model.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifAgent {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// StepObject — a single interaction turn in the trajectory.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifStep {
    pub step_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: String,
    pub message: AtifMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<AtifToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<AtifObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<AtifStepMetrics>,
    /// Who took the step and the delegation step that started it
    /// (TB-2, AGE-859).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<AtifStepExtra>,
}

/// Chatty-specific step data, carried in `steps[].extra`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifStepExtra {
    /// The agent that took the step, as its path in the tree:
    /// `root/coder-1/tester-0` (AGE-859).
    pub agent: String,
    /// The `step_id` of the delegation step that started this agent;
    /// absent on the root's steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_step: Option<u32>,
    /// On a `best_of` step: the attempt kept, how, the judge's reason and
    /// which child agent judged (AGE-853, AGE-859).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_of: Option<serde_json::Value>,
    /// On the step that stands in for a conversation over the capture cap:
    /// the size of what was cut (RC-0). The agent's tool calls follow it,
    /// from the swarm trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_bytes: Option<u64>,
}

/// Message field — either a plain string or an array of ContentPart (v1.6 multimodal).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AtifMessage {
    Text(String),
    Parts(Vec<AtifContentPart>),
}

impl Serialize for AtifMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AtifMessage::Text(s) => serializer.serialize_str(s),
            AtifMessage::Parts(parts) => parts.serialize(serializer),
        }
    }
}

/// ContentPartSchema (v1.6) — text or image content.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AtifContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AtifImageSource },
}

/// ImageSourceSchema (v1.6) — image reference with MIME type.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifImageSource {
    pub media_type: String,
    pub path: String,
}

/// ToolCallSchema — a single tool invocation.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: serde_json::Value,
    /// The plugin a plugin tool's call belongs to (TB-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<AtifToolCallExtra>,
}

/// Chatty-specific tool call data, carried in `tool_calls[].extra`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifToolCallExtra {
    pub plugin: String,
}

/// ObservationSchema — results from tool calls or other actions.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifObservation {
    pub results: Vec<AtifObservationResult>,
}

/// ObservationResultSchema — a single result within an observation.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifObservationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The call failed and `content` is its error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

/// MetricsSchema — per-step token usage and cost.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifStepMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// FinalMetricsSchema — aggregate metrics for the entire trajectory.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifFinalMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    /// Metrics ATIF has no field for. Absent when there is nothing to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<AtifFinalMetricsExtra>,
}

/// Chatty-specific aggregate metrics, carried in `final_metrics.extra`.
///
/// The prompt-cache counts are what makes the hit rate reconstructable from an
/// export: `total_prompt_tokens` already folds them in, so without these there
/// is no way to tell a cached prompt from an uncached one. A field is `None`,
/// not `0`, when the provider reported no cache activity — the two are
/// different claims (AGE-278).
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifFinalMetricsExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u32>,
}

impl AtifFinalMetricsExtra {
    /// `None` when neither count was reported, so the block is omitted rather
    /// than written as zeroes.
    pub fn from_totals(cache_read: u32, cache_write: u32) -> Option<Self> {
        (cache_read > 0 || cache_write > 0).then_some(Self {
            cache_read_tokens: (cache_read > 0).then_some(cache_read),
            cache_write_tokens: (cache_write > 0).then_some(cache_write),
        })
    }
}

/// Custom extra block for Chatty-specific data (feedback, regenerations).
/// The ATIF spec allows arbitrary data in `extra` fields.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifExtra {
    #[serde(default)]
    pub feedback: Vec<Option<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regenerations: Vec<AtifRegeneration>,
    /// Every agent of the run, parents before children (TB-2, AGE-859).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<Vec<AtifSwarmAgent>>,
    /// Why this export is not the whole run, when it is not: step usage
    /// that does not sum to `final_metrics`, a delegation step with no
    /// child trajectory, a conversation cut or never captured (AGE-859).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<String>,
}

/// One agent of a swarm export: a node of its `SwarmTrace`, less its tool
/// calls, which are the export's steps.
#[derive(Debug, Serialize, Deserialize)]
pub struct AtifSwarmAgent {
    pub name: String,
    /// Its path in the tree, as its steps' `extra.agent` names it.
    pub path: String,
    pub spec: String,
    /// The index of its parent in the roster; `None` for the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    pub turns: u32,
    pub text_bytes: u64,
    /// Its own spend, one line per model and plugin; never a price.
    pub usage: Vec<UsageLine>,
    pub status: NodeStatus,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AtifRegeneration {
    pub message_index: usize,
    pub original_text: String,
    pub timestamp: i64,
}
