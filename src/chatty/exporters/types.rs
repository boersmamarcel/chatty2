use serde::Serialize;

/// Top-level ATIF (Agent Trajectory Interchange Format) export structure.
///
/// Spec: <https://github.com/laude-institute/harbor/blob/main/docs/rfcs/0001-trajectory-format.md>
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
pub struct AtifAgent {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// StepObject — a single interaction turn in the trajectory.
#[derive(Debug, Serialize)]
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
}

/// Message field — either a plain string or an array of ContentPart (v1.6 multimodal).
#[derive(Debug)]
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
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum AtifContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AtifImageSource },
}

/// ImageSourceSchema (v1.6) — image reference with MIME type.
#[derive(Debug, Serialize)]
pub struct AtifImageSource {
    pub media_type: String,
    pub path: String,
}

/// ToolCallSchema — a single tool invocation.
#[derive(Debug, Serialize)]
pub struct AtifToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: serde_json::Value,
}

/// ObservationSchema — results from tool calls or other actions.
#[derive(Debug, Serialize)]
pub struct AtifObservation {
    pub results: Vec<AtifObservationResult>,
}

/// ObservationResultSchema — a single result within an observation.
#[derive(Debug, Serialize)]
pub struct AtifObservationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// MetricsSchema — per-step token usage and cost.
#[derive(Debug, Serialize)]
pub struct AtifStepMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// FinalMetricsSchema — aggregate metrics for the entire trajectory.
#[derive(Debug, Serialize)]
pub struct AtifFinalMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
}

/// Custom extra block for Chatty-specific data (feedback, regenerations).
/// The ATIF spec allows arbitrary data in `extra` fields.
#[derive(Debug, Serialize)]
pub struct AtifExtra {
    pub feedback: Vec<Option<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regenerations: Vec<AtifRegeneration>,
}

#[derive(Debug, Serialize)]
pub struct AtifRegeneration {
    pub message_index: usize,
    pub original_text: String,
    pub timestamp: i64,
}
