use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const LEGACY_FALLBACK_MODULE_DIR: &str = ".chatty/modules";

/// Settings for the WASM module runtime and protocol gateway.
///
/// `deny_unknown_fields`: BI-7 (PL-D5, no backward compatibility) — there is
/// no `loopback_roles` option and never was one to keep reading; a field
/// this shape does not know is refused by name rather than silently
/// dropped, exactly as `virtual_agents`' old shape already was.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// The broker's virtual agents (ADR-0011 C10), by agent spec name
    /// (AGE-614): each names a spec in `<workspace>/.chatty/agents/`, the
    /// data directory's `chatty/agents/`, or the presets. Empty means
    /// `local-agent` and every exposed spec of your own, plus the presets
    /// they delegate to
    /// ([`agent_spec::exposed_specs`](crate::agent_spec::exposed_specs),
    /// PL-U5, AGE-760); naming agents here sets the roster to exactly them,
    /// which is how a preset joins without its team.
    ///
    /// Roles are declared here rather than passed on `invoke_agent`, so the
    /// leader's tool schema and prompt stay identical whatever the team. A
    /// file still listing agent objects (the old `VirtualAgentConfig`
    /// shape) fails to load, naming this field.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_roster"
    )]
    pub virtual_agents: Vec<String>,
    /// What the whole team shares, as opposed to what one agent does
    /// (AGE-406). Absent in a settings file written before it existed, and
    /// then exactly the empty declaration.
    #[serde(default, skip_serializing_if = "TeamConfig::is_empty")]
    pub team: TeamConfig,
}

/// Settings that belong to the declared team rather than to any one of its
/// members (ADR-0011 C12).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamConfig {
    /// The command the runner runs in a worker's worktree once its task
    /// ends, whose exit code and last lines go into the evidence envelope
    /// the leader reads — e.g.
    /// `"python3 -m unittest discover -s tests -t . -v"`.
    ///
    /// Run with the platform shell in the worktree, not through the agent's
    /// tools, so it is the *runner's* fact and not the worker's account of
    /// one. `None` leaves the envelope with branch, commits and diff stat
    /// only. It is never run for a worker whose profile has no shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// Whether each worker gets a `git worktree` of its own (ADR-0012)
    /// rather than the conversation's workspace: a `--team` run's
    /// `team.json` `isolate` (AGE-822), for an agent no team claims by name.
    /// Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub isolate: bool,
}

impl TeamConfig {
    /// Nothing declared — so a settings file that never had a `team` block
    /// round-trips without gaining one.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `virtual_agents` as spec names. An agent object — the `VirtualAgentConfig`
/// shape before agent specs — is refused with where its fields went, not
/// read (no backward compatibility, PL-D2).
fn deserialize_roster<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    Vec::<serde_json::Value>::deserialize(deserializer)?
        .into_iter()
        .map(|entry| match entry {
            serde_json::Value::String(name) => Ok(name),
            serde_json::Value::Object(_) => Err(D::Error::custom(
                "virtual_agents lists an agent object (the old VirtualAgentConfig shape), \
                 which is no longer read: write each agent as a spec in \
                 .chatty/agents/<name>.toml and list its name here",
            )),
            other => Err(D::Error::custom(format!(
                "virtual_agents entries are agent spec names, got {other}"
            ))),
        })
        .collect()
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

    /// The names the root is offered from the broker's virtual agents,
    /// looked up from `workspace`: what was declared, or the default roster
    /// when nothing was, less team-internal specs
    /// ([`agent_spec::roster_names`](crate::agent_spec::roster_names)).
    pub fn roster_names(&self, workspace: Option<&std::path::Path>) -> Vec<String> {
        self.roster_names_from(workspace, dirs::data_dir().as_deref())
    }

    /// [`Self::roster_names`] with the data directory given rather than the
    /// platform's, so a test can see a "global" spec without touching the
    /// real `dirs::data_dir()` (AGE-814).
    pub fn roster_names_from(
        &self,
        workspace: Option<&std::path::Path>,
        data_dir: Option<&std::path::Path>,
    ) -> Vec<String> {
        crate::agent_spec::roster_names_from(&self.virtual_agents, workspace, data_dir)
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

fn default_endpoint_budget() -> usize {
    1
}

impl Default for ModuleSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false,
            module_dir: default_module_dir(),
            default_endpoint_budget: default_endpoint_budget(),
            endpoint_budgets: HashMap::new(),
            virtual_agents: Vec::new(),
            team: TeamConfig::default(),
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
        assert!(!settings.module_dir.is_empty());
    }

    #[test]
    fn serde_roundtrip() {
        let original = ModuleSettingsModel {
            enabled: true,
            module_dir: "/custom/modules".to_string(),
            ..ModuleSettingsModel::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ModuleSettingsModel = serde_json::from_str(&json).unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.module_dir, "/custom/modules");
    }

    #[test]
    fn serde_defaults_on_empty_object() {
        let restored: ModuleSettingsModel = serde_json::from_str("{}").unwrap();
        assert!(!restored.enabled);
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

        let old: ModuleSettingsModel = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(old.default_endpoint_budget, 1);
        assert!(old.endpoint_budgets.is_empty());
    }

    /// PL-U5: nothing declared means the default roster, `local-agent`
    /// first.
    #[test]
    fn no_declared_virtual_agents_means_every_exposed_spec() {
        let settings = ModuleSettingsModel::default();
        assert!(settings.virtual_agents.is_empty());
        assert_eq!(
            settings.roster_names(None),
            crate::agent_spec::roster_names(&[], None),
            "nothing declared is the default roster"
        );
        assert_eq!(settings.roster_names(None)[0], "local-agent");

        let old: ModuleSettingsModel = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(old.virtual_agents.is_empty());
    }

    #[test]
    fn declared_virtual_agents_survive_a_roundtrip_and_name_themselves() {
        let original = ModuleSettingsModel {
            virtual_agents: vec!["local-coder".to_string(), "local-reviewer".to_string()],
            ..ModuleSettingsModel::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(
            json.contains(r#""virtual_agents":["local-coder","local-reviewer"]"#),
            "{json}"
        );
        let restored: ModuleSettingsModel = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.virtual_agents, original.virtual_agents);
        assert_eq!(
            restored.roster_names(None),
            vec!["local-coder".to_string(), "local-reviewer".to_string()],
            "declared names replace the default roster rather than joining it"
        );
    }

    /// AGE-614, no backward compatibility: a settings file still declaring
    /// agents as objects fails to load, naming the field and where agents
    /// are declared now.
    #[test]
    fn old_virtual_agents_config_is_refused() {
        let err = serde_json::from_str::<ModuleSettingsModel>(
            r#"{"virtual_agents":[{"name":"local-coder","model":"qwen3:4b","extra_args":[]}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("virtual_agents"), "{err}");
        assert!(err.contains(".chatty/agents/"), "{err}");

        let err = serde_json::from_str::<ModuleSettingsModel>(r#"{"virtual_agents":[7]}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("virtual_agents"), "{err}");
    }

    /// BI-7 (PL-D5, no backward compatibility): there is no `loopback_roles`
    /// option — a settings file naming one fails to load rather than being
    /// read and ignored, exactly as an unknown field always has here.
    #[test]
    fn loopback_has_no_role_option() {
        let err = serde_json::from_str::<ModuleSettingsModel>(r#"{"loopback_roles":true}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("loopback_roles"), "{err}");
    }

    /// AGE-406 Do item 3: `team.verification` is optional, round-trips, and
    /// a file written before it existed neither gains it nor changes.
    #[test]
    fn the_teams_verification_command_is_optional_and_round_trips() {
        let declared: ModuleSettingsModel = serde_json::from_str(
            r#"{"team":{"verification":"python3 -m unittest discover -s tests -t . -v"}}"#,
        )
        .unwrap();
        assert_eq!(
            declared.team.verification.as_deref(),
            Some("python3 -m unittest discover -s tests -t . -v")
        );
        let round_tripped: ModuleSettingsModel =
            serde_json::from_str(&serde_json::to_string(&declared).unwrap()).unwrap();
        assert_eq!(round_tripped.team, declared.team);

        let old: ModuleSettingsModel = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(old.team.verification.is_none());
        assert!(
            !serde_json::to_string(&old).unwrap().contains("team"),
            "a settings file that never had a team block does not gain one"
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
