use gpui::Global;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct GeneralSettingsModel {
    pub font_size: f32,
    pub line_height: f32,
    pub theme_name: Option<String>,
    pub dark_mode: Option<bool>,
}

impl Default for GeneralSettingsModel {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            line_height: 20.0,
            theme_name: None,
            dark_mode: None,
        }
    }
}

impl Global for GeneralSettingsModel {}
