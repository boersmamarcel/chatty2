//! One export entry point for a whole run (AGE-860).
//!
//! The desktop and `chatty-tui` export through [`export_run`]: the root
//! conversation's steps, with every agent it delegated to nested under the
//! delegation step that started it (AGE-859). The agents come from the root
//! conversation itself: each turn that delegated keeps them on its trace.

use anyhow::Result;

use super::atif_exporter::run_to_atif;
use crate::repositories::ConversationData;
use crate::settings::models::models_store::ModelConfig;

/// Everything an export covers: the root conversation, whose turns carry
/// their agents.
pub struct ExportRun<'a> {
    pub root: &'a ConversationData,
    pub model_config: Option<&'a ModelConfig>,
}

/// Render `run` as pretty-printed ATIF JSON.
pub fn export_run(run: &ExportRun<'_>) -> Result<String> {
    let atif = run_to_atif(run)?;
    Ok(serde_json::to_string_pretty(&atif)?)
}
