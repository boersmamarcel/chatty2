use serde::Serialize;

/// Top-level ATIF (Agent Trajectory Interchange Format) export structure.
///
/// See <https://harborframework.com/docs/agents/trajectory-format>
#[derive(Debug, Serialize)]
pub struct AtifExport {
    pub session_id: String,
    pub agent: AtifAgent,
    pub steps: Vec<AtifStep>,
    pub final_metrics: AtifFinalMetrics,
    pub extra: AtifExtra,
}

#[derive(Debug, Serialize)]
pub struct AtifAgent {
    pub model_name: String,
    pub provider: String,
}

#[derive(Debug, Serialize)]
pub struct AtifStep {
    pub source: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AtifAttachment>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AtifToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<AtifStepMetrics>,
}

#[derive(Debug, Serialize)]
pub struct AtifAttachment {
    #[serde(rename = "type")]
    pub attachment_type: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct AtifToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AtifStepMetrics {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Serialize)]
pub struct AtifFinalMetrics {
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct AtifExtra {
    pub feedback: Vec<Option<String>>,
    pub regenerations: Vec<AtifRegeneration>,
}

#[derive(Debug, Serialize)]
pub struct AtifRegeneration {
    pub message_index: usize,
    pub original_text: String,
    pub timestamp: i64,
}
