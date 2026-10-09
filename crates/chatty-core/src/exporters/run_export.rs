//! One export entry point for a whole run (AGE-860).
//!
//! The desktop and `chatty-tui` export through [`export_run`]. Today a run is
//! its root conversation; sub-agent steps join it here (AGE-859), so neither
//! front end changes when they do.

use anyhow::Result;

use super::atif_exporter::conversation_to_atif;
use crate::repositories::ConversationData;
use crate::settings::models::models_store::ModelConfig;

/// Everything an export covers. AGE-859 adds the workers' conversations.
pub struct ExportRun<'a> {
    pub root: &'a ConversationData,
    pub model_config: Option<&'a ModelConfig>,
}

/// Render `run` as pretty-printed ATIF JSON.
pub fn export_run(run: &ExportRun<'_>) -> Result<String> {
    let atif = conversation_to_atif(run.root, run.model_config)?;
    Ok(serde_json::to_string_pretty(&atif)?)
}
