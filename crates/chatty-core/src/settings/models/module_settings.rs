use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const LEGACY_FALLBACK_MODULE_DIR: &str = ".chatty/modules";

/// Settings for the WASM module runtime and protocol gateway.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// The broker's virtual agents (ADR-0011 C10): each is a name the leader
    /// can delegate to, with its own model and tool set. Empty means the one
    /// default worker, `local-agent`, which runs the roster's default model
    /// with the leader's tools.
    ///
    /// Roles are declared here rather than passed on `invoke_agent`, so the
    /// leader's tool schema and prompt stay identical whatever the team.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub virtual_agents: Vec<VirtualAgentConfig>,
}

/// One named virtual agent the broker publishes (ADR-0011 C10).
///
/// Each becomes a worker runner whose children get `--model <model>` when
/// set, `--disable <groups>` when set, then `extra_args`, on top of the
/// flags every worker gets.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VirtualAgentConfig {
    /// The name callers address at `/a2a/{name}`, e.g. `local-reviewer`.
    pub name: String,
    /// The model the worker runs, as `chatty-tui --model` resolves it (id,
    /// name, or a substring of the model identifier). `None` leaves the
    /// child to resolve the roster's default, as an undeclared worker does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tool groups the worker runs without, as `chatty-tui --disable` names
    /// them: `shell`, `fs-read`, `fs-write`, `fetch`, `git`, `code-exec`,
    /// `docker-exec`. Ignored when `tools` names a profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disable_tools: Vec<String>,
    /// The role's standing instructions, appended to the worker's system
    /// prompt (ADR-0011 C11). Without one, a reviewer only knows it is a
    /// reviewer if the leader says so in the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
    /// The named tool profile the worker runs — `coordinator`, `coder` or
    /// `reviewer` (`chatty_core::factories::tool_profile`). An allowlist of
    /// tool *names*, where `disable_tools` removes whole groups; it wins
    /// when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    /// Any further `chatty-tui` flags, appended verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
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

    /// The names the broker publishes as virtual agents: what was declared,
    /// or the one default worker when nothing was.
    pub fn virtual_agent_names(&self) -> Vec<String> {
        if self.virtual_agents.is_empty() {
            vec![crate::tools::LOCAL_AGENT_NAME.to_string()]
        } else {
            self.virtual_agents.iter().map(|a| a.name.clone()).collect()
        }
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
            virtual_agents: Vec::new(),
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

    /// ADR-0011 C10: nothing declared means the one default worker, so a
    /// settings file written before virtual agents existed publishes exactly
    /// what it did before.
    #[test]
    fn no_declared_virtual_agents_means_the_one_default_worker() {
        let settings = ModuleSettingsModel::default();
        assert!(settings.virtual_agents.is_empty());
        assert_eq!(
            settings.virtual_agent_names(),
            vec!["local-agent".to_string()]
        );

        let old: ModuleSettingsModel =
            serde_json::from_str(r#"{"enabled":true,"gateway_port":8420}"#).unwrap();
        assert!(old.virtual_agents.is_empty());
    }

    #[test]
    fn declared_virtual_agents_survive_a_roundtrip_and_name_themselves() {
        let original = ModuleSettingsModel {
            virtual_agents: vec![
                VirtualAgentConfig {
                    name: "local-coder".to_string(),
                    model: Some("qwen3:4b".to_string()),
                    ..VirtualAgentConfig::default()
                },
                VirtualAgentConfig {
                    name: "local-reviewer".to_string(),
                    model: Some("gemma4:26b".to_string()),
                    disable_tools: vec!["fs-write".into(), "shell".into(), "git".into()],
                    preamble: Some("You review, you do not edit.".to_string()),
                    tools: Some("reviewer".to_string()),
                    extra_args: vec!["--enable".into(), "fetch".into()],
                },
            ],
            ..ModuleSettingsModel::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ModuleSettingsModel = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.virtual_agents, original.virtual_agents);
        assert_eq!(
            restored.virtual_agent_names(),
            vec!["local-coder".to_string(), "local-reviewer".to_string()],
            "declared names replace the default worker rather than joining it"
        );

        // A role survives the file as written (ADR-0011 C11).
        let reviewer = &restored.virtual_agents[1];
        assert_eq!(reviewer.tools.as_deref(), Some("reviewer"));
        assert_eq!(
            reviewer.preamble.as_deref(),
            Some("You review, you do not edit.")
        );

        // Neither is written out when unset, so a pre-C11 file round-trips
        // byte for byte.
        let coder = serde_json::to_string(&original.virtual_agents[0]).unwrap();
        assert!(!coder.contains("preamble"), "{coder}");
        assert!(!coder.contains("tools"), "{coder}");

        // The schema the docs promise: a declaration needs only a name.
        let minimal: ModuleSettingsModel =
            serde_json::from_str(r#"{"virtual_agents":[{"name":"local-coder"}]}"#).unwrap();
        assert_eq!(
            minimal.virtual_agents,
            vec![VirtualAgentConfig {
                name: "local-coder".to_string(),
                ..VirtualAgentConfig::default()
            }]
        );
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
