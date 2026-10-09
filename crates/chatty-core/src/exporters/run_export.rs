//! One export entry point for a whole run (AGE-860).
//!
//! The desktop and `chatty-tui` both export through [`export_run`], in ATIF
//! or JSONL. Today a run is its root conversation; sub-agent steps join it
//! here (AGE-859), nested under `agent` / `parent_step`, so neither front end
//! changes when they do.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::atif_exporter::conversation_to_atif;
use crate::repositories::ConversationData;
use crate::settings::models::models_store::ModelConfig;

/// How a run ended; written on the final JSONL line of each agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    Failed,
    Stopped,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Stopped => "stopped",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Atif,
    Jsonl,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "atif" => Some(Self::Atif),
            "jsonl" => Some(Self::Jsonl),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Atif => "atif.json",
            Self::Jsonl => "jsonl",
        }
    }
}

/// Everything an export covers. AGE-859 adds the workers' conversations.
pub struct ExportRun<'a> {
    pub root: &'a ConversationData,
    pub model_config: Option<&'a ModelConfig>,
    pub status: RunStatus,
}

/// Render `run` in `format`. JSONL is one line per ATIF step, in order, each
/// carrying `agent` (path, `"root"` for the root), `parent_step` (`null` for
/// the root), `seq` (global order) and `status` (`null` except on an agent's
/// final line).
pub fn export_run(run: &ExportRun<'_>, format: ExportFormat) -> Result<String> {
    let atif = conversation_to_atif(run.root, run.model_config)?;
    match format {
        ExportFormat::Atif => Ok(serde_json::to_string_pretty(&atif)?),
        ExportFormat::Jsonl => {
            let steps = atif
                .get("steps")
                .and_then(Value::as_array)
                .context("ATIF has no steps array")?;
            let mut lines = Vec::new();
            let last = steps.len().saturating_sub(1);
            for (seq, step) in steps.iter().enumerate() {
                let mut obj = match step {
                    Value::Object(m) => m.clone(),
                    _ => anyhow::bail!("ATIF step is not an object"),
                };
                obj.insert("agent".into(), json!("root"));
                obj.insert("parent_step".into(), Value::Null);
                obj.insert("seq".into(), json!(seq));
                let status = (seq == last).then(|| run.status.as_str());
                obj.insert("status".into(), json!(status));
                lines.push(serde_json::to_string(&obj)?);
            }
            if steps.is_empty() {
                lines.push(serde_json::to_string(&json!({
                    "agent": "root", "parent_step": null, "seq": 0,
                    "status": run.status.as_str(),
                }))?);
            }
            Ok(lines.join("\n") + "\n")
        }
    }
}
