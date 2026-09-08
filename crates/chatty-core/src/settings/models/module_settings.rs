use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const LEGACY_FALLBACK_MODULE_DIR: &str = ".chatty/modules";

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
    /// How many delegated workers the broker runs at once against one model
    /// endpoint when nothing more specific is known (ADR-0011 C6).
    ///
    /// One, because the endpoint that needs a budget is a local model server
    /// and a local model server that has not said otherwise serves one
    /// request at a time; more workers than that on it queue inside the
    /// server and evict each other's weights.
    #[serde(default = "default_endpoint_budget")]
    pub default_endpoint_budget: usize,
    /// Per-endpoint overrides, keyed by the model server's base URL as
    /// `ProviderConfig::endpoint_key` writes it (e.g.
    /// `http://localhost:11434`). Beats both the default and whatever the
    /// provider reports about itself.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub endpoint_budgets: HashMap<String, usize>,
}

impl ModuleSettingsModel {
    /// The number of workers allowed on `endpoint` at once.
    ///
    /// `reported` is what the provider says about itself —
    /// `ProviderConfig::parallel_requests`, i.e. Ollama's parallel-request
    /// setting where it is known. An explicit override wins over it, and the
    /// configured default is the answer when neither exists. Never zero: a
    /// budget of zero is not a budget, it is a deadlock.
    pub fn endpoint_budget(&self, endpoint: &str, reported: Option<usize>) -> usize {
        self.endpoint_budgets
            .get(endpoint)
            .copied()
            .or(reported)
            .unwrap_or(self.default_endpoint_budget)
            .max(1)
    }
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
        .unwrap_or_else(|| LEGACY_FALLBACK_MODULE_DIR.to_string())
}

pub fn normalize_module_dir(module_dir: String) -> String {
    let trimmed = module_dir.trim();
    if trimmed.is_empty() {
        return default_module_dir();
    }

    let platform_default = default_module_dir();
    if trimmed == LEGACY_FALLBACK_MODULE_DIR && platform_default != LEGACY_FALLBACK_MODULE_DIR {
        platform_default
    } else {
        trimmed.to_string()
    }
}

fn default_gateway_port() -> u16 {
    8420
}

fn default_endpoint_budget() -> usize {
    1
}

impl Default for ModuleSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            module_dir: default_module_dir(),
            gateway_port: default_gateway_port(),
            default_endpoint_budget: default_endpoint_budget(),
            endpoint_budgets: HashMap::new(),
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
            ..ModuleSettingsModel::default()
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

    #[test]
    fn an_endpoint_with_nothing_said_about_it_gets_one_worker() {
        let settings = ModuleSettingsModel::default();
        assert_eq!(settings.default_endpoint_budget, 1);
        assert_eq!(settings.endpoint_budget("http://localhost:11434", None), 1);
    }

    #[test]
    fn what_the_provider_reports_beats_the_default() {
        let settings = ModuleSettingsModel::default();
        assert_eq!(
            settings.endpoint_budget("http://localhost:11434", Some(4)),
            4
        );
    }

    #[test]
    fn an_explicit_override_beats_what_the_provider_reports() {
        let mut settings = ModuleSettingsModel::default();
        settings
            .endpoint_budgets
            .insert("http://localhost:11434".to_string(), 2);

        assert_eq!(
            settings.endpoint_budget("http://localhost:11434", Some(4)),
            2
        );
        assert_eq!(
            settings.endpoint_budget("https://openrouter.ai/api/v1", Some(4)),
            4,
            "the override is for one endpoint, not for all of them"
        );
    }

    #[test]
    fn a_budget_of_zero_is_read_as_one_rather_than_a_deadlock() {
        let mut settings = ModuleSettingsModel {
            default_endpoint_budget: 0,
            ..ModuleSettingsModel::default()
        };
        settings
            .endpoint_budgets
            .insert("http://localhost:11434".to_string(), 0);

        assert_eq!(settings.endpoint_budget("http://localhost:11434", None), 1);
        assert_eq!(settings.endpoint_budget("anything-else", None), 1);
    }

    #[test]
    fn budgets_survive_a_roundtrip_and_default_on_settings_written_before_them() {
        let mut original = ModuleSettingsModel::default();
        original
            .endpoint_budgets
            .insert("http://box:11434".into(), 3);
        let json = serde_json::to_string(&original).unwrap();
        let restored: ModuleSettingsModel = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.endpoint_budgets.get("http://box:11434"), Some(&3));

        let old: ModuleSettingsModel =
            serde_json::from_str(r#"{"enabled":true,"gateway_port":8420}"#).unwrap();
        assert_eq!(old.default_endpoint_budget, 1);
        assert!(old.endpoint_budgets.is_empty());
    }

    #[test]
    fn normalize_module_dir_migrates_legacy_fallback_when_platform_default_exists() {
        let normalized = normalize_module_dir(LEGACY_FALLBACK_MODULE_DIR.to_string());
        let platform_default = default_module_dir();

        if platform_default == LEGACY_FALLBACK_MODULE_DIR {
            assert_eq!(normalized, LEGACY_FALLBACK_MODULE_DIR);
        } else {
            assert_eq!(normalized, platform_default);
        }
    }

    #[test]
    fn normalize_module_dir_replaces_empty_values() {
        assert_eq!(
            normalize_module_dir("   ".to_string()),
            default_module_dir()
        );
    }
}
