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
    /// The desktop's terminal dock (AGE-582). `#[serde(default)]` for the
    /// same reason as `sidebar_mode`.
    #[serde(default)]
    pub terminal: TerminalSettings,
}

/// Default scrollback of a dock terminal, in lines.
pub const DEFAULT_TERMINAL_SCROLLBACK: usize = 10_000;
/// Default height of the terminal dock, in pixels.
pub const DEFAULT_TERMINAL_DOCK_HEIGHT: f32 = 300.0;

/// Settings of the desktop terminal dock (AGE-582). Fonts apply to open
/// terminals at once; the shell and scrollback to terminals opened after
/// the change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// Monospace family; `None` uses the theme's code font.
    pub font_family: Option<String>,
    /// Font size in pixels; `None` uses the theme's code font size.
    pub font_size: Option<f32>,
    /// Program to run instead of the platform's default shell.
    pub shell: Option<String>,
    /// Lines of scrollback per terminal.
    pub scrollback_lines: usize,
    /// Height the dock opens at, in pixels. Dragging the dock's top edge
    /// updates it.
    pub dock_height: f32,
    /// What the eye icon on a terminal tab does (AGE-583): ask each time, or
    /// share at a remembered level without asking.
    pub share_default: TerminalShareDefault,
}

/// "When sharing a terminal" (AGE-583): the level the share dialog's
/// "Remember this" saved, or `Ask`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalShareDefault {
    /// Open the share dialog every time.
    #[default]
    Ask,
    /// Share read-only without asking.
    ReadOnly,
    /// Share read + run without asking.
    ReadRun,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: None,
            shell: None,
            scrollback_lines: DEFAULT_TERMINAL_SCROLLBACK,
            dock_height: DEFAULT_TERMINAL_DOCK_HEIGHT,
            share_default: TerminalShareDefault::Ask,
        }
    }
}

impl Default for GeneralSettingsModel {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            theme_name: None,
            dark_mode: None,
            sidebar_mode: SidebarMode::default(),
            terminal: TerminalSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings file saved before the terminal dock existed still loads,
    /// with the dock's defaults.
    #[test]
    fn settings_without_terminal_section_load_with_defaults() {
        let json = r#"{"font_size":15.0,"theme_name":null,"dark_mode":true}"#;
        let loaded: GeneralSettingsModel = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.font_size, 15.0);
        assert_eq!(loaded.terminal, TerminalSettings::default());
        assert_eq!(loaded.terminal.scrollback_lines, 10_000);
    }

    /// A partial terminal section keeps the other fields' defaults.
    #[test]
    fn partial_terminal_section_fills_in_defaults() {
        let json = r#"{"font_size":14.0,"terminal":{"shell":"/bin/zsh"}}"#;
        let loaded: GeneralSettingsModel = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.terminal.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(loaded.terminal.dock_height, DEFAULT_TERMINAL_DOCK_HEIGHT);
        assert_eq!(
            loaded.terminal.scrollback_lines,
            DEFAULT_TERMINAL_SCROLLBACK
        );
        assert_eq!(loaded.terminal.share_default, TerminalShareDefault::Ask);
    }

    /// The remembered share level round-trips in snake_case.
    #[test]
    fn share_default_round_trips() {
        let json = r#"{"font_size":14.0,"terminal":{"share_default":"read_run"}}"#;
        let loaded: GeneralSettingsModel = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.terminal.share_default, TerminalShareDefault::ReadRun);
        let saved = serde_json::to_value(&loaded).unwrap();
        assert_eq!(saved["terminal"]["share_default"], "read_run");
    }
}
