use gpui::Global;
use serde::{Deserialize, Serialize};

/// Settings for training data collection and export
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TrainingSettingsModel {
    /// Auto-export conversations as ATIF JSON after each completed assistant response.
    /// Opt-in: disabled by default.
    #[serde(default)]
    pub atif_auto_export: bool,
    /// Auto-export conversations as JSONL (SFT + DPO) after each completed assistant response.
    /// Opt-in: disabled by default.
    #[serde(default)]
    pub jsonl_auto_export: bool,
}

impl Global for TrainingSettingsModel {}
