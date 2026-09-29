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
//! grants = ["llm"]              # a subset of what it requests (PL-U4)
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
    ("data-analyst", include_str!("../agents/data-analyst.toml")),
    ("data-lead", include_str!("../agents/data-lead.toml")),
    ("editor", include_str!("../agents/editor.toml")),
    ("researcher", include_str!("../agents/researcher.toml")),
    ("reviewer", include_str!("../agents/reviewer.toml")),
    ("writer", include_str!("../agents/writer.toml")),
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
    /// Capabilities this agent grants the plugin: a subset of what its
    /// `metadata` requests (PL-U4). `logging` is always granted; nothing
    /// else is unless listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
    /// What the plugin's `config::get` reads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "PluginLimits::is_empty")]
    pub limits: PluginLimits,
}

/// A capability a spec grants a plugin (PL-D3, PL-U4). Written as its WIT
/// name — `llm`, `config`, `logging`, `file`, `billing` — or `file:<root>`.
///
/// * `llm` runs completions on the calling agent's model. It costs money, so
///   its usage counts against the turn's budget, but it asks no approval.
/// * `config` reads the plugin's `[config]` (the module's, with the spec's
///   `config` on top).
/// * `logging` is always granted; listing it changes nothing.
/// * `file` reads below the module's own `[files] root`; `file:<root>` reads
///   below `<root>` (an absolute path) instead. Read-only either way.
/// * `billing` reserves and settles Hive credits.
///
/// No v1 capability is side-effecting ([`Grant::side_effecting`]); `http` is
/// not a capability in v1 (PL-D3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Grant {
    Llm,
    Config,
    Logging,
    /// `root` is `None` for `file`: the module's `[files] root`.
    File {
        root: Option<PathBuf>,
    },
    Billing,
}

/// The grants a spec may write, for error messages.
const GRANT_NAMES: &str = "llm, config, logging, file, file:<root>, billing";

impl Grant {
    /// The WIT capability this grant links.
    pub fn capability(&self) -> chatty_wasm_runtime::Capability {
        use chatty_wasm_runtime::Capability;
        match self {
            Self::Llm => Capability::Llm,
            Self::Config => Capability::Config,
            Self::Logging => Capability::Logging,
            Self::File { .. } => Capability::File,
            Self::Billing => Capability::Billing,
        }
    }

    /// Whether using it changes something outside the plugin, so each call
    /// needs the user's approval under the agent's approval mode (PL-U4 §4).
    /// None of v1's does: `file` is read-only, and `llm` and `billing` cost
    /// money but are counted against the budget instead. A future `http` or
    /// `file-write` would be.
    pub fn side_effecting(&self) -> bool {
        match self {
            Self::Llm | Self::Config | Self::Logging | Self::File { .. } | Self::Billing => false,
        }
    }
}

impl std::str::FromStr for Grant {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(root) = text.strip_prefix("file:") {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(format!(
                    "grant `{text}`: the root of `file:<root>` must be an absolute path"
                ));
            }
            return Ok(Self::File { root: Some(root) });
        }
        match text {
            "llm" => Ok(Self::Llm),
            "config" => Ok(Self::Config),
            "logging" => Ok(Self::Logging),
            "file" => Ok(Self::File { root: None }),
            "billing" => Ok(Self::Billing),
            other => Err(format!(
                "`{other}` is not a capability a plugin can be granted (valid: {GRANT_NAMES})"
            )),
        }
    }
}

impl std::fmt::Display for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File { root: Some(root) } => write!(f, "file:{}", root.display()),
            other => f.write_str(other.capability().name()),
        }
    }
}

impl TryFrom<String> for Grant {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<Grant> for String {
    fn from(grant: Grant) -> Self {
        grant.to_string()
    }
}

impl PluginSpec {
    /// Its grants by name, comma-separated: `llm, config`; `nothing` for
    /// none.
    pub fn grant_list(&self) -> String {
        if self.grants.is_empty() {
            return "nothing".to_string();
        }
        self.grants
            .iter()
            .map(Grant::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// What it requests against what it is granted, as the settings pages
    /// show it (PL-U4): `requests llm, config · grants llm`. `requested`
    /// is its `metadata`'s, by WIT name, `None` when the plugin has not
    /// loaded. A grant it does not request is called out: the agent will
    /// not build with it.
    pub fn capabilities_summary(&self, requested: Option<&[String]>) -> String {
        let grants = format!("grants {}", self.grant_list());
        let Some(requested) = requested else {
            return format!("{grants} · requests unknown (the plugin has not loaded)");
        };
        let requests = if requested.is_empty() {
            "nothing".to_string()
        } else {
            requested.join(", ")
        };
        let unrequested: Vec<String> = self
            .grants
            .iter()
            .filter(|grant| {
                **grant != Grant::Logging
                    && !requested.iter().any(|r| r == grant.capability().name())
            })
            .map(Grant::to_string)
            .collect();
        let mut summary = format!("requests {requests} · {grants}");
        if !unrequested.is_empty() {
            summary.push_str(&format!(
                " · not requested: {} (the agent will not build)",
                unrequested.join(", ")
            ));
        }
        summary
    }
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

/// `[swarm]`: who it may call and who may call it (PL-D4), as
/// [`services::delegation_policy`](crate::services::delegation_policy)
/// reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmSection {
    /// Agents (names or `*` globs) this agent may delegate to. Empty: it is
    /// offered neither `invoke_agent` nor `list_agents`, whatever its tool
    /// profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegates_to: Vec<String>,
    /// Whether others may call it. Defaults to `true`.
    #[serde(default = "exposed_default", skip_serializing_if = "is_exposed")]
    pub exposed: bool,
    /// When set, only these callers (names or `*` globs) may call it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callers: Option<Vec<String>>,
}

impl Default for SwarmSection {
    fn default() -> Self {
        Self {
            delegates_to: Vec::new(),
            exposed: exposed_default(),
            callers: None,
        }
    }
}

fn exposed_default() -> bool {
    true
}

fn is_exposed(exposed: &bool) -> bool {
    *exposed
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
    /// A grant the plugin does not request. Found when the plugin loads:
    /// what it requests is in its `metadata`.
    UnrequestedGrant(String),
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
            Self::UnrequestedGrant(why) => write!(f, "plugins: {why}"),
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
            let file_grants = plugin
                .grants
                .iter()
                .filter(|grant| matches!(grant, Grant::File { .. }))
                .count();
            if file_grants > 1 {
                errors.push(SpecError::BadPlugin(format!(
                    "`{}` is granted `file` more than once; grant one root",
                    plugin.module
                )));
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

/// The specs behind a roster of names. An empty roster is every exposed
/// spec the workspace can reach ([`exposed_specs`]): one kind of agent,
/// whatever file or preset defined it (PL-U5).
pub fn load_roster(names: &[String], workspace: Option<&Path>) -> Result<Vec<AgentSpec>> {
    load_roster_from(names, workspace, dirs::data_dir().as_deref())
}

/// [`load_roster`] with the data directory given rather than the
/// platform's.
pub fn load_roster_from(
    names: &[String],
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
) -> Result<Vec<AgentSpec>> {
    if names.is_empty() {
        return Ok(exposed_specs_from(workspace, data_dir));
    }
    names
        .iter()
        .map(|name| load_agent_spec_from(name, workspace, data_dir).map(|loaded| loaded.spec))
        .collect()
}

/// The names of the roster [`load_roster`] loads: `declared` when it names
/// any agents, else every exposed spec's name.
pub fn roster_names(declared: &[String], workspace: Option<&Path>) -> Vec<String> {
    roster_names_from(declared, workspace, dirs::data_dir().as_deref())
}

/// [`roster_names`] with the data directory given rather than the
/// platform's.
pub fn roster_names_from(
    declared: &[String],
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
) -> Vec<String> {
    roster_names_of(declared, &inspect_agent_specs_from(workspace, data_dir))
}

/// [`roster_names`] over listings already read: `declared` when it names
/// any agents, else `local-agent` and every served listing's name.
pub fn roster_names_of(declared: &[String], listings: &[SpecListing]) -> Vec<String> {
    if !declared.is_empty() {
        return declared.to_vec();
    }
    let default_name = crate::tools::LOCAL_AGENT_NAME;
    let mut names = vec![default_name.to_string()];
    names.extend(
        listings
            .iter()
            .filter(|listing| listing.is_served() && listing.name != default_name)
            .map(|listing| listing.name.clone()),
    );
    names
}

/// The workspace a conversation's own roster and the broker's roster must
/// both resolve from (AGE-719, PL-U5b): the conversation's own working
/// directory when it has one, else the process's shared default. The
/// broker (refreshed from the shared default, or the active conversation's
/// working directory when it has one) and a conversation's own agent build
/// call this with the same two inputs, so neither can privately pick a
/// different workspace for the same conversation.
pub fn roster_workspace<'a>(
    default_workspace: Option<&'a Path>,
    conversation_working_dir: Option<&'a Path>,
) -> Option<&'a Path> {
    conversation_working_dir.or(default_workspace)
}

/// Whether a conversation may safely publish the names [`roster_workspace`]
/// found for it as its `local_agents`.
///
/// `broker_is_live` is whether a broker/gateway is actually running right
/// now; `broker_workspace` is the workspace it was last built to serve
/// (meaningless when no broker is live). No broker yet means nothing to
/// disagree with — the roster names are the best guess available until one
/// starts. Once a broker *is* running, though, its workspace is ground
/// truth: a conversation whose own resolved workspace does not match it
/// must not list names the live broker cannot reach (the "Do" section of
/// AGE-719 — fail loudly rather than list an agent that can't be reached).
pub fn roster_workspace_matches(
    conversation_workspace: Option<&Path>,
    broker_workspace: Option<&Path>,
    broker_is_live: bool,
) -> bool {
    !broker_is_live || conversation_workspace == broker_workspace
}

/// Every spec a workspace can reach that others may call, in lookup order
/// and once per name: `local-agent` first (a bare spec unless a spec
/// directory defines one), then each first definition whose
/// `swarm.exposed` is set. A file that does not load is left out with a
/// warning rather than taking the whole roster down; the Agents settings
/// page and `/agents` show it with its error.
pub fn exposed_specs(workspace: Option<&Path>) -> Vec<AgentSpec> {
    exposed_specs_from(workspace, dirs::data_dir().as_deref())
}

/// [`exposed_specs`] with the data directory given rather than the
/// platform's.
pub fn exposed_specs_from(workspace: Option<&Path>, data_dir: Option<&Path>) -> Vec<AgentSpec> {
    let default_name = crate::tools::LOCAL_AGENT_NAME;
    let mut specs = Vec::new();
    for listing in inspect_agent_specs_from(workspace, data_dir) {
        if listing.shadowed {
            continue;
        }
        match listing.spec {
            Ok(spec) if spec.swarm.exposed => specs.push(spec),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                agent = %listing.name,
                %error,
                "Leaving an agent spec that does not load out of the roster"
            ),
        }
    }
    match specs
        .iter()
        .position(|spec| spec.agent.name == default_name)
    {
        Some(at) => {
            let spec = specs.remove(at);
            specs.insert(0, spec);
        }
        None => specs.insert(0, AgentSpec::named(default_name)),
    }
    specs
}

/// One spec file or preset as the Agents settings page and `/agents` show
/// it: where it was found, whether a nearer one shadows it, and what it
/// declares — or why it does not load.
#[derive(Clone, Debug, PartialEq)]
pub struct SpecListing {
    pub name: String,
    pub source: SpecSource,
    pub shadowed: bool,
    pub spec: Result<AgentSpec, String>,
}

impl SpecListing {
    /// Whether the broker serves it: the name's first definition, loaded,
    /// and exposed.
    pub fn is_served(&self) -> bool {
        !self.shadowed && self.spec.as_ref().is_ok_and(|spec| spec.swarm.exposed)
    }
}

impl SpecSource {
    /// Where it was found, for a person: the file, or `preset`.
    pub fn label(&self) -> String {
        match self {
            Self::Workspace(path) | Self::DataDir(path) => path.display().to_string(),
            Self::Preset => "preset".to_string(),
        }
    }
}

/// Every spec a workspace can reach, each loaded from its own file (a
/// shadowed one too), in [`list_agent_specs`] order.
pub fn inspect_agent_specs(workspace: Option<&Path>) -> Vec<SpecListing> {
    inspect_agent_specs_from(workspace, dirs::data_dir().as_deref())
}

/// [`inspect_agent_specs`] with the data directory given rather than the
/// platform's.
pub fn inspect_agent_specs_from(
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
) -> Vec<SpecListing> {
    list_agent_specs_from(workspace, data_dir)
        .into_iter()
        .map(|listed| {
            let spec = match &listed.source {
                SpecSource::Workspace(path) | SpecSource::DataDir(path) => {
                    std::fs::read_to_string(path)
                        .with_context(|| format!("failed to read {}", path.display()))
                        .and_then(|text| parse_named(&listed.name, &text))
                }
                SpecSource::Preset => PRESETS
                    .iter()
                    .find(|(preset, _)| *preset == listed.name)
                    .map(|(_, text)| parse_named(&listed.name, text))
                    .expect("a listed preset exists"),
            }
            .map_err(|error| format!("{error:#}"));
            SpecListing {
                name: listed.name,
                source: listed.source,
                shadowed: listed.shadowed,
                spec,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::providers_store::ProviderType;

    const FULL: &str = r#"
[agent]
name = "auditor"
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
delegates_to = ["data-analyst", "*-reviewer"]
exposed = true
callers = ["data-lead"]

[budget]
max_agent_turns = 0
max_duration = "30m"
cap_usd = 2.0
"#;

    #[test]
    fn agent_spec_round_trips_toml_and_json() {
        let spec = AgentSpec::from_toml(FULL).expect("the full example parses");
        assert_eq!(spec.agent.name, "auditor");
        assert_eq!(spec.tools.profile.as_deref(), Some("reviewer"));
        assert_eq!(spec.plugins[0].config["threshold"], "0.05");
        assert_eq!(spec.plugins[0].limits.max_memory_mb, Some(64));
        assert_eq!(spec.swarm.callers.as_deref().unwrap(), ["data-lead"]);
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

        // `exposed` defaults to true; only `false` is written.
        assert!(bare.swarm.exposed);
        let mut hidden = bare.clone();
        hidden.swarm.exposed = false;
        let json = hidden.to_json().unwrap();
        assert!(json.contains(r#""exposed":false"#), "{json}");
        assert_eq!(AgentSpec::from_json(&json).unwrap(), hidden);
    }

    /// PL-U4: grants are capabilities by WIT name, or `file:<root>` with an
    /// absolute root; anything else (`http` included, not in v1) fails the
    /// parse naming the valid ones, and two file roots fail validation.
    #[test]
    fn plugin_grants_are_capabilities() {
        let root = std::env::temp_dir().join("weights");
        let file_root = format!("file:{}", root.display());
        let spec = AgentSpec::from_toml(&format!(
            "[agent]\nname = \"a\"\n[[plugins]]\nmodule = \"m\"\n\
             grants = [\"llm\", \"config\", \"logging\", \"billing\", {file_root:?}]\n"
        ))
        .expect("every v1 capability parses");
        assert_eq!(
            spec.plugins[0].grants,
            [
                Grant::Llm,
                Grant::Config,
                Grant::Logging,
                Grant::Billing,
                Grant::File { root: Some(root) },
            ]
        );
        let back = AgentSpec::from_toml(&spec.to_toml().unwrap()).unwrap();
        assert_eq!(back, spec, "grants round-trip as their names");

        for (grant, why) in [
            ("http", "`http` is not a capability a plugin can be granted"),
            ("file:weights", "must be an absolute path"),
        ] {
            let err = AgentSpec::from_toml(&format!(
                "[agent]\nname = \"a\"\n[[plugins]]\nmodule = \"m\"\ngrants = [{grant:?}]\n"
            ))
            .unwrap_err()
            .to_string();
            assert!(err.contains(why), "{grant}: {err}");
        }

        let mut spec = AgentSpec::named("a");
        spec.plugins = vec![PluginSpec {
            module: "m".to_string(),
            grants: vec![
                Grant::File { root: None },
                Grant::File {
                    root: Some(std::env::temp_dir()),
                },
            ],
            ..PluginSpec::default()
        }];
        let err = spec.validate(None).unwrap_err().to_string();
        assert!(
            err.contains("`m` is granted `file` more than once"),
            "{err}"
        );
    }

    /// PL-U4 §5: the settings pages show requested against granted, and
    /// call out a grant the plugin does not request.
    #[test]
    fn plugin_capabilities_summary_shows_requested_against_granted() {
        let mut plugin = PluginSpec {
            module: "benford".to_string(),
            grants: vec![Grant::Llm],
            ..PluginSpec::default()
        };
        let requested = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            plugin.capabilities_summary(Some(&requested(&["llm", "config"]))),
            "requests llm, config · grants llm"
        );
        assert_eq!(
            plugin.capabilities_summary(None),
            "grants llm · requests unknown (the plugin has not loaded)"
        );
        plugin.grants = vec![Grant::Logging, Grant::File { root: None }];
        assert_eq!(
            plugin.capabilities_summary(Some(&[])),
            "requests nothing · grants logging, file · not requested: file (the agent will not \
             build)"
        );
        plugin.grants.clear();
        assert_eq!(
            plugin.capabilities_summary(Some(&requested(&["config"]))),
            "requests config · grants nothing"
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
            let path = dir.join("data-analyst.toml");
            std::fs::write(
                &path,
                format!("[agent]\nname = \"data-analyst\"\npreamble = \"{preamble}\"\n"),
            )
            .unwrap();
            path
        };

        let loaded =
            load_agent_spec_from("data-analyst", Some(workspace.path()), Some(data.path()))
                .unwrap();
        assert_eq!(loaded.source, SpecSource::Preset);

        let data_path = write(&data_dir, "data");
        let loaded =
            load_agent_spec_from("data-analyst", Some(workspace.path()), Some(data.path()))
                .unwrap();
        assert_eq!(loaded.source, SpecSource::DataDir(data_path.clone()));
        assert_eq!(loaded.spec.agent.preamble.as_deref(), Some("data"));

        let ws_path = write(&ws_dir, "workspace");
        let loaded =
            load_agent_spec_from("data-analyst", Some(workspace.path()), Some(data.path()))
                .unwrap();
        assert_eq!(loaded.source, SpecSource::Workspace(ws_path.clone()));
        assert_eq!(loaded.spec.agent.preamble.as_deref(), Some("workspace"));

        let listed: Vec<_> = list_agent_specs_from(Some(workspace.path()), Some(data.path()))
            .into_iter()
            .filter(|entry| entry.name == "data-analyst")
            .collect();
        assert_eq!(
            listed,
            vec![
                ListedSpec {
                    name: "data-analyst".to_string(),
                    source: SpecSource::Workspace(ws_path),
                    shadowed: false,
                },
                ListedSpec {
                    name: "data-analyst".to_string(),
                    source: SpecSource::DataDir(data_path),
                    shadowed: true,
                },
                ListedSpec {
                    name: "data-analyst".to_string(),
                    source: SpecSource::Preset,
                    shadowed: true,
                },
            ]
        );
        let reviewer = list_agent_specs_from(Some(workspace.path()), Some(data.path()))
            .into_iter()
            .find(|entry| entry.name == "reviewer")
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

    /// AGE-752: what ships reads well in Settings → Agents and `list_agents`:
    /// every preset has a one-line description and a short preamble, names
    /// no model (the roster's default runs it), and has a turn budget.
    #[test]
    fn every_preset_is_described_and_budgeted() {
        for (name, _) in PRESETS {
            let spec = load_agent_spec_from(name, None, None).unwrap().spec;
            let description = spec.agent.description.as_deref().unwrap_or_default();
            assert!(
                !description.is_empty() && !description.contains('\n') && description.len() <= 130,
                "{name}: {description:?}"
            );
            let preamble = spec.agent.preamble.as_deref().unwrap_or_default();
            assert!(
                (1..=1000).contains(&preamble.len()),
                "{name}: a preamble of {} bytes",
                preamble.len()
            );
            assert!(spec.agent.model.is_none(), "{name} names no model");
            assert!(
                spec.budget.max_agent_turns.is_some(),
                "{name} has a turn budget"
            );
        }
    }

    fn names(specs: &[AgentSpec]) -> Vec<&str> {
        specs.iter().map(|spec| spec.agent.name.as_str()).collect()
    }

    /// PL-U5: with nothing declared, the roster is `local-agent` and every
    /// exposed spec — the presets included, so `data-analyst` is an agent
    /// like any other.
    #[test]
    fn an_empty_roster_is_every_exposed_spec() {
        let roster = load_roster_from(&[], None, None).unwrap();
        assert_eq!(
            names(&roster),
            [
                crate::tools::LOCAL_AGENT_NAME,
                "data-analyst",
                "data-lead",
                "editor",
                "researcher",
                "reviewer",
                "writer",
            ]
        );
        assert_eq!(roster[0], AgentSpec::named(crate::tools::LOCAL_AGENT_NAME));
        assert_eq!(
            roster_names_from(&[], None, None),
            names(&roster)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_declared_roster_is_exactly_what_it_names() {
        let declared = vec!["reviewer".to_string()];
        let roster = load_roster_from(&declared, None, None).unwrap();
        assert_eq!(names(&roster), ["reviewer"]);
        assert_eq!(roster_names_from(&declared, None, None), declared);
        assert!(load_roster_from(&["nope".to_string()], None, None).is_err());
    }

    /// Workspace specs join the roster, shadow what they rename, and stay
    /// out when they are not exposed or do not load.
    #[test]
    fn the_exposed_roster_follows_the_spec_directories() {
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("analyst.toml"),
            "[agent]\nname = \"analyst\"\ndescription = \"mine\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("hidden.toml"),
            "[agent]\nname = \"hidden\"\n\n[swarm]\nexposed = false\n",
        )
        .unwrap();
        std::fs::write(dir.join("broken.toml"), "[agent]\nnme = \"broken\"\n").unwrap();
        std::fs::write(
            dir.join("local-agent.toml"),
            "[agent]\nname = \"local-agent\"\npreamble = \"Be brief.\"\n",
        )
        .unwrap();
        // A workspace spec that turns a preset off hides the preset too.
        std::fs::write(
            dir.join("data-analyst.toml"),
            "[agent]\nname = \"data-analyst\"\n\n[swarm]\nexposed = false\n",
        )
        .unwrap();

        let listings = inspect_agent_specs_from(Some(workspace.path()), None);
        let listing = |name: &str| {
            listings
                .iter()
                .find(|listing| listing.name == name && !listing.shadowed)
                .unwrap()
        };
        assert!(listing("analyst").is_served());
        assert!(!listing("hidden").is_served());
        let broken = listing("broken");
        assert!(!broken.is_served());
        assert!(
            broken.spec.as_ref().unwrap_err().contains("nme"),
            "the error names the bad field: {broken:?}"
        );
        let shadowed_preset = listings
            .iter()
            .find(|listing| listing.name == "data-analyst" && listing.shadowed)
            .unwrap();
        assert_eq!(shadowed_preset.source, SpecSource::Preset);
        assert!(shadowed_preset.spec.is_ok());
        assert!(!shadowed_preset.is_served());

        let roster = exposed_specs_from(Some(workspace.path()), None);
        assert_eq!(
            names(&roster),
            [
                "local-agent",
                "analyst",
                "data-lead",
                "editor",
                "researcher",
                "reviewer",
                "writer",
            ]
        );
        assert_eq!(roster[0].agent.preamble.as_deref(), Some("Be brief."));
    }

    /// AGE-719 (PL-U5b): `roster_workspace` is the one place that picks
    /// between a conversation's own working directory and the shared
    /// default — the broker and a conversation's own agent build must call
    /// it with the same two inputs to ever agree.
    #[test]
    fn roster_workspace_prefers_the_conversation_s_own_directory() {
        let default_ws = Path::new("/default");
        let conv_ws = Path::new("/conversation-own-dir");
        assert_eq!(
            roster_workspace(Some(default_ws), Some(conv_ws)),
            Some(conv_ws)
        );
        assert_eq!(roster_workspace(Some(default_ws), None), Some(default_ws));
        assert_eq!(roster_workspace(None, None), None);
    }

    #[test]
    fn roster_workspace_matches_is_permissive_before_a_broker_exists() {
        let a = Path::new("/a");
        let b = Path::new("/b");
        // No broker running yet: nothing to disagree with.
        assert!(roster_workspace_matches(Some(a), Some(b), false));
        assert!(roster_workspace_matches(Some(a), None, false));
        // A live broker built for a different workspace: disagreement.
        assert!(!roster_workspace_matches(Some(a), Some(b), true));
        assert!(!roster_workspace_matches(Some(a), None, true));
        assert!(!roster_workspace_matches(None, Some(b), true));
        // A live broker built for the same workspace: agreement.
        assert!(roster_workspace_matches(Some(a), Some(a), true));
        assert!(roster_workspace_matches(None, None, true));
    }

    /// AGE-719 (PL-U5b), test named by the issue: a conversation whose own
    /// working directory defines a spec the shared/default workspace does
    /// not lists it — because `roster_workspace` resolves to *its* own
    /// directory, the same directory `load_roster`/`roster_names` (what the
    /// broker itself scans from, PL-U5) then reads. Before AGE-719, the
    /// conversation-build path read the shared default directly and never
    /// consulted the conversation's own working directory at all, so a
    /// per-chat spec was invisible to `roster_names` even though a
    /// conversation whose *default* happened to equal its own directory
    /// would have found it — i.e., the two paths could read different
    /// workspaces for the same conversation.
    #[test]
    fn per_chat_workspace_specs_are_served() {
        let default_workspace = tempfile::tempdir().unwrap();
        let conversation_workspace = tempfile::tempdir().unwrap();
        let conv_agents_dir = conversation_workspace.path().join(WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&conv_agents_dir).unwrap();
        std::fs::write(
            conv_agents_dir.join("per-chat-agent.toml"),
            "[agent]\nname = \"per-chat-agent\"\n",
        )
        .unwrap();

        // The shared default workspace has no such spec.
        let default_names = roster_names_from(&[], Some(default_workspace.path()), None);
        assert!(
            !default_names.contains(&"per-chat-agent".to_string()),
            "the default workspace must not see the conversation's own spec: {default_names:?}"
        );

        // The conversation's own working directory does — resolved through
        // the one function both the broker and the conversation build call.
        let resolved = roster_workspace(
            Some(default_workspace.path()),
            Some(conversation_workspace.path()),
        );
        assert_eq!(resolved, Some(conversation_workspace.path()));
        let conversation_names = roster_names_from(&[], resolved, None);
        assert!(
            conversation_names.contains(&"per-chat-agent".to_string()),
            "the conversation's own workspace must serve its own spec: {conversation_names:?}"
        );

        // `invoke_agent` reaches it: `load_roster` (what a real task
        // dispatch loads the spec from) resolves the same spec by name.
        let loaded = load_roster_from(&["per-chat-agent".to_string()], resolved, None).unwrap();
        assert_eq!(names(&loaded), ["per-chat-agent"]);
    }

    /// AGE-719 (PL-U5b), test named by the issue: two conversations with
    /// different workspaces each see exactly their own workspace's roster —
    /// no cross-conversation leakage — and `roster_workspace_matches` is the
    /// gate that keeps a conversation from listing a name a live broker,
    /// built for some *other* workspace, does not actually serve.
    #[test]
    fn roster_and_list_agree() {
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        for (dir, agent_name) in [
            (workspace_a.path(), "alpha-agent"),
            (workspace_b.path(), "beta-agent"),
        ] {
            let agents_dir = dir.join(WORKSPACE_AGENTS_DIR);
            std::fs::create_dir_all(&agents_dir).unwrap();
            std::fs::write(
                agents_dir.join(format!("{agent_name}.toml")),
                format!("[agent]\nname = \"{agent_name}\"\n"),
            )
            .unwrap();
        }

        let names_a = roster_names_from(&[], Some(workspace_a.path()), None);
        let names_b = roster_names_from(&[], Some(workspace_b.path()), None);
        assert!(names_a.contains(&"alpha-agent".to_string()));
        assert!(!names_a.contains(&"beta-agent".to_string()));
        assert!(names_b.contains(&"beta-agent".to_string()));
        assert!(!names_b.contains(&"alpha-agent".to_string()));

        // Conversation A's own list_agents agrees with a broker actually
        // built for workspace A ...
        assert!(roster_workspace_matches(
            Some(workspace_a.path()),
            Some(workspace_a.path()),
            true,
        ));
        // ... but must refuse to agree — fail loudly, not silently list an
        // unreachable agent — against a broker built for workspace B while
        // conversation A is the one being served.
        assert!(!roster_workspace_matches(
            Some(workspace_a.path()),
            Some(workspace_b.path()),
            true,
        ));
    }
}
