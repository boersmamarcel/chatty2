use serde::{Deserialize, Serialize};

/// Browser engine settings model.
///
/// Persisted to `~/.config/chatty/browser_settings.json` via the
/// `BrowserSettingsRepository`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserSettingsModel {
    /// Whether the browser engine is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Run the browser in headless mode (no visible window for browsing).
    /// Session capture always uses a visible window regardless of this setting.
    #[serde(default)]
    pub headless: bool,

    /// Maximum number of concurrent tabs.
    #[serde(default = "default_max_tabs")]
    pub max_tabs: u32,

    /// Page load timeout in seconds.
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,

    /// Require user approval before browser auth actions.
    #[serde(default = "default_true")]
    pub require_auth_approval: bool,

    /// Require user approval before browser interaction actions
    /// (click, fill, select).
    #[serde(default = "default_true")]
    pub require_action_approval: bool,
}

fn default_max_tabs() -> u32 {
    5
}
fn default_timeout_seconds() -> u32 {
    30
}
fn default_true() -> bool {
    true
}

impl Default for BrowserSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            headless: false,
            max_tabs: default_max_tabs(),
            timeout_seconds: default_timeout_seconds(),
            require_auth_approval: true,
            require_action_approval: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = BrowserSettingsModel::default();
        assert!(!settings.enabled);
        assert!(!settings.headless);
        assert_eq!(settings.max_tabs, 5);
        assert_eq!(settings.timeout_seconds, 30);
        assert!(settings.require_auth_approval);
        assert!(settings.require_action_approval);
    }

    #[test]
    fn test_serde_roundtrip() {
        let settings = BrowserSettingsModel {
            enabled: true,
            headless: true,
            max_tabs: 3,
            timeout_seconds: 60,
            require_auth_approval: false,
            require_action_approval: true,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let decoded: BrowserSettingsModel = serde_json::from_str(&json).unwrap();
        assert!(decoded.enabled);
        assert!(decoded.headless);
        assert_eq!(decoded.max_tabs, 3);
        assert_eq!(decoded.timeout_seconds, 60);
        assert!(!decoded.require_auth_approval);
        assert!(decoded.require_action_approval);
    }

    #[test]
    fn test_serde_defaults_for_missing_fields() {
        let json = r#"{"enabled": true}"#;
        let decoded: BrowserSettingsModel = serde_json::from_str(json).unwrap();
        assert!(decoded.enabled);
        assert!(!decoded.headless);
        assert_eq!(decoded.max_tabs, 5);
        assert_eq!(decoded.timeout_seconds, 30);
        assert!(decoded.require_auth_approval);
    }
}
