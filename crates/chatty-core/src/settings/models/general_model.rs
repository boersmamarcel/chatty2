use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct GeneralSettingsModel {
    pub font_size: f32,
    pub theme_name: Option<String>,
    pub dark_mode: Option<bool>,
    #[serde(default)]
    pub show_tool_traces_live: bool,
}

impl Default for GeneralSettingsModel {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            theme_name: None,
            dark_mode: None,
            show_tool_traces_live: false,
        }
    }
}
