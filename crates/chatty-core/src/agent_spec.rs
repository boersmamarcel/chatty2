//! `AgentSpec`: the one declarative definition of an agent (PL-D2, AGE-614).
//!
//! An agent used to be defined three ways — a `VirtualAgentConfig` in module
//! settings, a `team.json` leader/roster entry, and whatever a host built from
//! its own settings. A spec is that definition once, read the same way by
//! every host:
//!
//! ```toml
//! [agent]
//! name = "local-reviewer"
//! description = "Reviews a worker's branch"
//! model = "qwen3:4b"            # resolved like --model
//! preamble = "Verify, do not trust."
//!
//! [tools]
//! profile = "reviewer"          # a tool_profile name
//! disable = ["fs-write"]        # tool groups, narrows only
//! skills = ["coder-reviewer"]   # read_skill names
//!
//! [[plugins]]                   # its tools become the agent's (PL-U2)
//! module = "benford"
//! version = "^0.2"
//!
//! [swarm]
//! delegates_to = ["local-coder"]
//! exposed = true
//!
//! [budget]
//! max_agent_turns = 0           # 0 = uncapped, the deadline applies
//! max_duration = "30m"
//! cap_usd = 2.0                 # per task, feeds the spend gate
//! ```
//!
//! TOML on disk, the same shape as JSON on the wire. Unknown fields are an
//! error naming the field — `extra_args` in particular is gone, not ignored.
//!
//! Specs live in `<workspace>/.chatty/agents/<name>.toml`, then
//! `<data_dir>/chatty/agents/<name>.toml`, then the presets compiled into
//! this crate (`crates/chatty-core/agents/`). The first match wins; a
//! malformed file there is an error, not a fall-through.
//! [`AgentBuildContext::from_spec`](crate::factories::AgentBuildContext::from_spec)
//! turns one into what an agent is built with.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::factories::agent_factory::{tool_profile, tool_profile_names};
use crate::services::turn_budget::parse_duration;
use crate::settings::models::execution_settings::{
    TOOL_GROUPS, VALID_TOOL_GROUPS, canonical_tool_group,
};
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};

/// The relative directory specs live in under a workspace.
pub const WORKSPACE_AGENTS_DIR: &str = ".chatty/agents";

/// The specs compiled into the binary: `(name, spec.toml)`.
pub const PRESETS: &[(&str, &str)] = &[
    (
        "coder-reviewer-leader",
        include_str!("../agents/coder-reviewer-leader.toml"),
    ),
    ("local-coder", include_str!("../agents/local-coder.toml")),
    (
        "local-reviewer",
        include_str!("../agents/local-reviewer.toml"),
    ),
];

/// One agent, as declared.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub agent: AgentSection,
    #[serde(default, skip_serializing_if = "ToolsSection::is_empty")]
    pub tools: ToolsSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginSpec>,
    #[serde(default, skip_serializing_if = "SwarmSection::is_empty")]
    pub swarm: SwarmSection,
    #[serde(default, skip_serializing_if = "BudgetSection::is_empty")]
    pub budget: BudgetSection,
}

/// `[agent]`: who it is and what it runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// The A2A address, unique within a roster: `/a2a/{name}`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Resolved like `--model` (id, name, or a substring of the model
    /// identifier). `None` runs the roster's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The role's standing instructions, after the base system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
}

/// `[tools]`: what it may call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    /// A named tool profile (`coordinator`, `coder`, `reviewer`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Tool groups switched off on top of the profile; narrows only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disable: Vec<String>,
    /// Skills the role is told to `read_skill` before it starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

impl ToolsSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `[[plugins]]`: a WASM module this agent runs with. The factory loads one
/// instance per agent and registers its tools as `<module>__<tool>`
/// (`tools::plugin_tool`, PL-U2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSpec {
    /// Hive name or local module directory name.
    pub module: String,
    /// A semver requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Capabilities this agent grants the plugin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<String>,
    /// What the plugin's `config::get` reads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "PluginLimits::is_empty")]
    pub limits: PluginLimits,
}

/// A plugin's resource ceilings, named as `module.toml` names them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_execution_ms: Option<u64>,
}

impl PluginLimits {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `[swarm]`: who it may call and who may call it (PL-D4; not enforced yet).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmSection {
    /// Agents (names or globs) this agent may delegate to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegates_to: Vec<String>,
    /// Whether others may reach it over A2A.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exposed: bool,
    /// When set, only these callers (names or globs) may call it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callers: Option<Vec<String>>,
}

impl SwarmSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `[budget]`: ceilings on one task. A caller's remaining budget may lower
/// them at run time, never raise them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSection {
    /// Model turns; `0` is uncapped, with the deadline applying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_turns: Option<u32>,
    /// Wall-clock budget: seconds, or `90s`, `30m`, `2h`, `1h30m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<String>,
    /// Dollars one task may spend, checked by the spend gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_usd: Option<f64>,
}

impl BudgetSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One thing wrong with a spec.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecError {
    BadName(String),
    UnknownProfile(String),
    UnknownToolGroup(String),
    BadModel(String),
    DuplicatePlugin(String),
    BadPlugin(String),
    BadDuration(String),
    BadCap(f64),
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName(name) => write!(
                f,
                "agent.name '{name}' is not a name: use lowercase letters, digits, '-' and '_', \
                 starting with a letter or digit, at most 64 characters"
            ),
            Self::UnknownProfile(profile) => write!(
                f,
                "tools.profile '{profile}' is not a tool profile (valid: {})",
                tool_profile_names().join(", ")
            ),
            Self::UnknownToolGroup(group) => write!(
                f,
                "tools.disable names '{group}', which is not a tool group (valid: {VALID_TOOL_GROUPS})"
            ),
            Self::BadModel(model) => write!(f, "agent.model '{model}' names no configured model"),
            Self::DuplicatePlugin(module) => {
                write!(f, "plugins lists module '{module}' more than once")
            }
            Self::BadPlugin(why) => write!(f, "plugins: {why}"),
            Self::BadDuration(why) => write!(f, "budget.max_duration: {why}"),
            Self::BadCap(cap) => write!(f, "budget.cap_usd {cap} is not a positive amount"),
        }
    }
}

/// Every problem with a spec, reported together.
#[derive(Clone, Debug, PartialEq)]
pub struct SpecErrors(pub Vec<SpecError>);

impl std::fmt::Display for SpecErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let lines: Vec<String> = self.0.iter().map(|e| format!("  - {e}")).collect();
        write!(f, "invalid agent spec:\n{}", lines.join("\n"))
    }
}

impl std::error::Error for SpecErrors {}

fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl AgentSpec {
    /// A spec with only a name: the full tool set, the default model.
    pub fn named(name: &str) -> Self {
        Self {
            agent: AgentSection {
                name: name.to_string(),
                ..AgentSection::default()
            },
            ..Self::default()
        }
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string(self)?)
    }

    pub fn from_json(text: &str) -> Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// `budget.max_duration`, parsed. `None` when unset or invalid (an
    /// invalid one fails [`validate`](Self::validate)).
    pub fn max_duration(&self) -> Option<Duration> {
        parse_duration(self.budget.max_duration.as_deref()?).ok()
    }

    /// Check the spec, reporting every problem at once. With `models`, the
    /// model reference must also resolve among them.
    pub fn validate(&self, models: Option<&[ModelConfig]>) -> Result<(), SpecErrors> {
        let mut errors = Vec::new();
        if !is_valid_name(&self.agent.name) {
            errors.push(SpecError::BadName(self.agent.name.clone()));
        }
        if let Some(model) = self.agent.model.as_deref() {
            let resolves = match models {
                Some(models) => resolve_model_query(models, Some(model)).is_some(),
                None => true,
            };
            if model.trim().is_empty() || !resolves {
                errors.push(SpecError::BadModel(model.to_string()));
            }
        }
        if let Some(profile) = self.tools.profile.as_deref()
            && tool_profile(profile).is_none()
        {
            errors.push(SpecError::UnknownProfile(profile.to_string()));
        }
        for group in &self.tools.disable {
            if !TOOL_GROUPS.contains(&canonical_tool_group(group).as_str()) {
                errors.push(SpecError::UnknownToolGroup(group.clone()));
            }
        }
        let mut modules = HashSet::new();
        for plugin in &self.plugins {
            if plugin.module.trim().is_empty() {
                errors.push(SpecError::BadPlugin("a plugin names no module".to_string()));
            } else if !modules.insert(plugin.module.as_str()) {
                errors.push(SpecError::DuplicatePlugin(plugin.module.clone()));
            }
        }
        if let Some(duration) = self.budget.max_duration.as_deref()
            && let Err(why) = parse_duration(duration)
        {
            errors.push(SpecError::BadDuration(why));
        }
        if let Some(cap) = self.budget.cap_usd
            && !(cap.is_finite() && cap > 0.0)
        {
            errors.push(SpecError::BadCap(cap));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SpecErrors(errors))
        }
    }
}

/// Where a spec was found.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecSource {
    /// `<workspace>/.chatty/agents/`, this file.
    Workspace(PathBuf),
    /// `<data_dir>/chatty/agents/`, this file.
    DataDir(PathBuf),
    /// One of [`PRESETS`].
    Preset,
}

/// A spec and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSpec {
    pub spec: AgentSpec,
    pub source: SpecSource,
}

/// One entry of [`list_agent_specs`]. A name found in more than one place is
/// listed once per place; every entry but the first is `shadowed`.
#[derive(Clone, Debug, PartialEq)]
pub struct ListedSpec {
    pub name: String,
    pub source: SpecSource,
    pub shadowed: bool,
}

fn spec_dirs(workspace: Option<&Path>, data_dir: Option<&Path>) -> Vec<(PathBuf, bool)> {
    let mut dirs = Vec::new();
    if let Some(root) = workspace {
        dirs.push((root.join(WORKSPACE_AGENTS_DIR), true));
    }
    if let Some(root) = data_dir {
        dirs.push((root.join("chatty").join("agents"), false));
    }
    dirs
}

fn source_for(path: PathBuf, in_workspace: bool) -> SpecSource {
    if in_workspace {
        SpecSource::Workspace(path)
    } else {
        SpecSource::DataDir(path)
    }
}

/// Load the spec `name` from the workspace, the platform data directory, or
/// the presets, in that order.
pub fn load_agent_spec(name: &str, workspace: Option<&Path>) -> Result<LoadedSpec> {
    load_agent_spec_from(name, workspace, dirs::data_dir().as_deref())
}

/// [`load_agent_spec`] with the data directory given rather than the
/// platform's.
pub fn load_agent_spec_from(
    name: &str,
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
) -> Result<LoadedSpec> {
    if !is_valid_name(name) {
        bail!("{}", SpecError::BadName(name.to_string()));
    }
    let dirs = spec_dirs(workspace, data_dir);
    for (dir, in_workspace) in &dirs {
        let path = dir.join(format!("{name}.toml"));
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let spec = parse_named(name, &text)
            .with_context(|| format!("{} is not a valid agent spec", path.display()))?;
        return Ok(LoadedSpec {
            spec,
            source: source_for(path, *in_workspace),
        });
    }
    if let Some((_, text)) = PRESETS.iter().find(|(preset, _)| *preset == name) {
        let spec = parse_named(name, text).expect("a preset spec is valid");
        return Ok(LoadedSpec {
            spec,
            source: SpecSource::Preset,
        });
    }
    bail!(
        "no agent spec '{name}': looked in {}, and the presets are {}",
        dirs.iter()
            .map(|(dir, _)| dir.join(format!("{name}.toml")).display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        PRESETS
            .iter()
            .map(|(preset, _)| *preset)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse_named(name: &str, text: &str) -> Result<AgentSpec> {
    let spec = AgentSpec::from_toml(text)?;
    spec.validate(None)?;
    if spec.agent.name != name {
        bail!(
            "agent.name '{}' does not match the file's name '{name}'",
            spec.agent.name
        );
    }
    Ok(spec)
}

/// Every spec a workspace can reach, in lookup order: the workspace's, the
/// data directory's, then the presets. A name found twice is listed twice,
/// the later entry marked `shadowed`.
pub fn list_agent_specs(workspace: Option<&Path>) -> Vec<ListedSpec> {
    list_agent_specs_from(workspace, dirs::data_dir().as_deref())
}

/// [`list_agent_specs`] with the data directory given rather than the
/// platform's.
pub fn list_agent_specs_from(workspace: Option<&Path>, data_dir: Option<&Path>) -> Vec<ListedSpec> {
    let mut seen = HashSet::new();
    let mut listed = Vec::new();
    for (dir, in_workspace) in spec_dirs(workspace, data_dir) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            listed.push(ListedSpec {
                shadowed: !seen.insert(name.clone()),
                name,
                source: source_for(path, in_workspace),
            });
        }
    }
    for (name, _) in PRESETS {
        listed.push(ListedSpec {
            shadowed: !seen.insert(name.to_string()),
            name: name.to_string(),
            source: SpecSource::Preset,
        });
    }
    listed
}

/// The specs behind a roster of names, or the one default worker
/// (`local-agent`, a bare spec) when the roster is empty.
pub fn load_roster(names: &[String], workspace: Option<&Path>) -> Result<Vec<AgentSpec>> {
    if names.is_empty() {
        return Ok(vec![AgentSpec::named(crate::tools::LOCAL_AGENT_NAME)]);
    }
    names
        .iter()
        .map(|name| load_agent_spec(name, workspace).map(|loaded| loaded.spec))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::providers_store::ProviderType;

    const FULL: &str = r#"
[agent]
name = "benford-analyst"
description = "Audits number sets for Benford's-law anomalies"
model = "qwen3:4b"
preamble = "You audit financial datasets."

[tools]
profile = "reviewer"
disable = ["shell"]
skills = ["benford-audit"]

[[plugins]]
module = "benford"
version = "^0.2"
grants = ["llm"]
config = { threshold = "0.05" }
limits = { max_memory_mb = 64, max_execution_ms = 10000 }

[swarm]
delegates_to = ["local-coder", "*-reviewer"]
exposed = true
callers = ["coder-reviewer-leader"]

[budget]
max_agent_turns = 0
max_duration = "30m"
cap_usd = 2.0
"#;

    #[test]
    fn agent_spec_round_trips_toml_and_json() {
        let spec = AgentSpec::from_toml(FULL).expect("the full example parses");
        assert_eq!(spec.agent.name, "benford-analyst");
        assert_eq!(spec.tools.profile.as_deref(), Some("reviewer"));
        assert_eq!(spec.plugins[0].config["threshold"], "0.05");
        assert_eq!(spec.plugins[0].limits.max_memory_mb, Some(64));
        assert_eq!(
            spec.swarm.callers.as_deref().unwrap(),
            ["coder-reviewer-leader"]
        );
        assert_eq!(spec.max_duration(), Some(Duration::from_secs(1800)));
        assert_eq!(spec.budget.cap_usd, Some(2.0));
        spec.validate(None).expect("the full example is valid");

        let toml_again = AgentSpec::from_toml(&spec.to_toml().unwrap()).unwrap();
        assert_eq!(toml_again, spec);
        let json_again = AgentSpec::from_json(&spec.to_json().unwrap()).unwrap();
        assert_eq!(json_again, spec);

        // A bare spec writes nothing but its name.
        let bare = AgentSpec::named("local-agent");
        assert_eq!(
            bare.to_json().unwrap(),
            r#"{"agent":{"name":"local-agent"}}"#
        );
        assert_eq!(
            AgentSpec::from_toml(&bare.to_toml().unwrap()).unwrap(),
            bare
        );
    }

    #[test]
    fn agent_spec_rejects_extra_args() {
        for text in [
            "[agent]\nname = \"x\"\nextra_args = [\"--enable\", \"fetch\"]\n",
            "extra_args = [\"--enable\"]\n[agent]\nname = \"x\"\n",
        ] {
            let err = AgentSpec::from_toml(text).unwrap_err().to_string();
            assert!(err.contains("extra_args"), "{err}");
        }
        let err = AgentSpec::from_json(r#"{"agent":{"name":"x","extra_args":[]}}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("extra_args"), "{err}");

        // Through the loader, too: the file is refused, not run without it.
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("x.toml"),
            "[agent]\nname = \"x\"\nextra_args = [\"--enable\"]\n",
        )
        .unwrap();
        let err = format!(
            "{:#}",
            load_agent_spec_from("x", Some(workspace.path()), None).unwrap_err()
        );
        assert!(err.contains("extra_args"), "{err}");
    }

    #[test]
    fn agent_spec_reports_all_errors() {
        let spec = AgentSpec {
            agent: AgentSection {
                name: "Bad Name".to_string(),
                model: Some("no-such-model".to_string()),
                ..AgentSection::default()
            },
            tools: ToolsSection {
                profile: Some("wizard".to_string()),
                disable: vec!["shell".to_string(), "teleport".to_string()],
                skills: Vec::new(),
            },
            plugins: vec![
                PluginSpec {
                    module: "benford".to_string(),
                    ..PluginSpec::default()
                },
                PluginSpec {
                    module: "benford".to_string(),
                    ..PluginSpec::default()
                },
            ],
            swarm: SwarmSection::default(),
            budget: BudgetSection {
                max_duration: Some("soon".to_string()),
                cap_usd: Some(-1.0),
                ..BudgetSection::default()
            },
        };
        let models = [ModelConfig::new(
            "qwen3:4b".to_string(),
            "qwen3:4b".to_string(),
            ProviderType::Ollama,
            "qwen3:4b".to_string(),
        )];
        let errors = spec.validate(Some(&models)).unwrap_err().0;
        assert!(errors.contains(&SpecError::BadName("Bad Name".to_string())));
        assert!(errors.contains(&SpecError::BadModel("no-such-model".to_string())));
        assert!(errors.contains(&SpecError::UnknownProfile("wizard".to_string())));
        assert!(errors.contains(&SpecError::UnknownToolGroup("teleport".to_string())));
        assert!(errors.contains(&SpecError::DuplicatePlugin("benford".to_string())));
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SpecError::BadDuration(_)))
        );
        assert!(errors.contains(&SpecError::BadCap(-1.0)));
        assert_eq!(errors.len(), 7, "{errors:?}");

        let text = SpecErrors(errors).to_string();
        for field in [
            "agent.name",
            "agent.model",
            "tools.profile",
            "tools.disable",
            "plugins",
            "budget.max_duration",
            "budget.cap_usd",
        ] {
            assert!(text.contains(field), "{field} missing from: {text}");
        }
    }

    #[test]
    fn spec_lookup_order() {
        let workspace = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let ws_dir = workspace.path().join(WORKSPACE_AGENTS_DIR);
        let data_dir = data.path().join("chatty").join("agents");
        let write = |dir: &Path, preamble: &str| {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join("local-coder.toml");
            std::fs::write(
                &path,
                format!("[agent]\nname = \"local-coder\"\npreamble = \"{preamble}\"\n"),
            )
            .unwrap();
            path
        };

        let loaded =
            load_agent_spec_from("local-coder", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(loaded.source, SpecSource::Preset);

        let data_path = write(&data_dir, "data");
        let loaded =
            load_agent_spec_from("local-coder", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(loaded.source, SpecSource::DataDir(data_path.clone()));
        assert_eq!(loaded.spec.agent.preamble.as_deref(), Some("data"));

        let ws_path = write(&ws_dir, "workspace");
        let loaded =
            load_agent_spec_from("local-coder", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(loaded.source, SpecSource::Workspace(ws_path.clone()));
        assert_eq!(loaded.spec.agent.preamble.as_deref(), Some("workspace"));

        let listed: Vec<_> = list_agent_specs_from(Some(workspace.path()), Some(data.path()))
            .into_iter()
            .filter(|entry| entry.name == "local-coder")
            .collect();
        assert_eq!(
            listed,
            vec![
                ListedSpec {
                    name: "local-coder".to_string(),
                    source: SpecSource::Workspace(ws_path),
                    shadowed: false,
                },
                ListedSpec {
                    name: "local-coder".to_string(),
                    source: SpecSource::DataDir(data_path),
                    shadowed: true,
                },
                ListedSpec {
                    name: "local-coder".to_string(),
                    source: SpecSource::Preset,
                    shadowed: true,
                },
            ]
        );
        let reviewer = list_agent_specs_from(Some(workspace.path()), Some(data.path()))
            .into_iter()
            .find(|entry| entry.name == "local-reviewer")
            .unwrap();
        assert!(
            !reviewer.shadowed,
            "a preset nobody overrides is not shadowed"
        );

        let err = load_agent_spec_from("nope", Some(workspace.path()), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains(".chatty/agents/nope.toml"), "{err}");
        assert!(load_agent_spec_from("../x", None, None).is_err());
    }

    #[test]
    fn a_file_whose_name_disagrees_with_its_spec_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.toml"), "[agent]\nname = \"b\"\n").unwrap();
        let err = format!(
            "{:#}",
            load_agent_spec_from("a", Some(workspace.path()), None).unwrap_err()
        );
        assert!(err.contains("does not match"), "{err}");
    }

    #[test]
    fn every_preset_is_valid_and_named_after_itself() {
        for (name, _) in PRESETS {
            let loaded = load_agent_spec_from(name, None, None).unwrap();
            assert_eq!(loaded.spec.agent.name, *name);
        }
    }

    #[test]
    fn an_empty_roster_is_the_one_default_worker() {
        let roster = load_roster(&[], None).unwrap();
        assert_eq!(
            roster,
            vec![AgentSpec::named(crate::tools::LOCAL_AGENT_NAME)]
        );
    }
}
