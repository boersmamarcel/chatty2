use serde::{Deserialize, Serialize};

/// Which navigator the left sidebar shows (AGE-480): the conversation list,
/// or the active workspace's file tree. A global UI preference, persisted
/// like the theme and font size so it survives a restart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidebarMode {
    #[default]
    Chats,
    Files,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneralSettingsModel {
    pub font_size: f32,
    pub theme_name: Option<String>,
    pub dark_mode: Option<bool>,
    /// AGE-480. `#[serde(default)]` so settings files saved before this
    /// field existed still load, defaulting to `Chats`.
    #[serde(default)]
    pub sidebar_mode: SidebarMode,
}

impl Default for GeneralSettingsModel {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            theme_name: None,
            dark_mode: None,
            sidebar_mode: SidebarMode::default(),
        }
    }
}
