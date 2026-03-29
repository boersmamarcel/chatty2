use serde::{Deserialize, Serialize};

/// Settings for the WASM module runtime and protocol gateway.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModuleSettingsModel {
    /// Whether the module runtime is enabled. Defaults to `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Directory to scan for WASM modules.
    /// Defaults to the platform-native data directory:
    /// - macOS: `~/Library/Application Support/chatty/modules/`
    /// - Linux: `~/.local/share/chatty/modules/` (or `$XDG_DATA_HOME/chatty/modules/`)
    /// - Windows: `%APPDATA%\chatty\modules\`
    #[serde(default = "default_module_dir")]
    pub module_dir: String,
    /// TCP port for the local protocol gateway.
    /// Defaults to `8420`.
    #[serde(default = "default_gateway_port")]
    pub gateway_port: u16,
}

/// Returns the platform-native default module directory.
///
/// Uses `dirs::data_dir()` to resolve the OS-specific data directory, then
/// appends `chatty/modules`. Falls back to `.chatty/modules` if the platform
/// data directory cannot be determined.
///
/// - **macOS**: `~/Library/Application Support/chatty/modules`
/// - **Linux**: `~/.local/share/chatty/modules` (or `$XDG_DATA_HOME/chatty/modules`)
/// - **Windows**: `{FOLDERID_RoamingAppData}\chatty\modules`
pub fn default_module_dir() -> String {
    dirs::data_dir()
        .map(|d| {
            d.join("chatty")
                .join("modules")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| ".chatty/modules".to_string())
}

fn default_gateway_port() -> u16 {
    8420
}

impl Default for ModuleSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            module_dir: default_module_dir(),
            gateway_port: default_gateway_port(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_module_dir_uses_platform_path() {
        let dir = default_module_dir();
        // Should end with chatty/modules (or chatty\modules on Windows)
        assert!(
            dir.ends_with("chatty/modules") || dir.ends_with("chatty\\modules"),
            "Expected path ending with chatty/modules, got: {}",
            dir
        );
        // Should NOT be the relative fallback in a normal environment
        assert_ne!(dir, ".chatty/modules", "Should use platform-native path");
    }

    #[test]
    fn default_settings_have_correct_values() {
        let settings = ModuleSettingsModel::default();
        assert!(!settings.enabled);
        assert_eq!(settings.gateway_port, 8420);
        assert!(!settings.module_dir.is_empty());
    }

    #[test]
    fn serde_roundtrip() {
        let original = ModuleSettingsModel {
            enabled: true,
            module_dir: "/custom/modules".to_string(),
            gateway_port: 9999,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ModuleSettingsModel = serde_json::from_str(&json).unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.module_dir, "/custom/modules");
        assert_eq!(restored.gateway_port, 9999);
    }

    #[test]
    fn serde_defaults_on_empty_object() {
        let restored: ModuleSettingsModel = serde_json::from_str("{}").unwrap();
        assert!(!restored.enabled);
        assert_eq!(restored.gateway_port, 8420);
        // module_dir should use the platform default
        assert_eq!(restored.module_dir, default_module_dir());
    }
}
