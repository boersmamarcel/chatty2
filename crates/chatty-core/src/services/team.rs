//! The team directory: one place for a roster, the leader's role, the
//! verification command, the skill the leader follows and the turn budget
//! (ADR-0011 C13 / AGE-407).
//!
//! `team.json` is a thin file of agent spec names (AGE-614): the leader's
//! spec, the roster's, and what belongs to the team rather than to any one
//! agent — verification, skill, the leader's turn budget. The specs are
//! looked up as [`load_agent_spec`](crate::agent_spec::load_agent_spec)
//! looks them up, so a workspace spec shadows a preset one.
//!
//! A working team used to be spread over `module_settings.json` (the
//! roster), `execution_settings.json` (`max_agent_turns`), a skill under the
//! data directory and a shell script's flags. `teams/<id>/team.json`, with
//! `SKILL.md` beside it, is that in one directory a Harbor arm can upload
//! and a run can reproduce. `chatty-tui --team <id>` loads it; nothing here
//! is persisted, the team applies to that run only.
//!
//! Search order: `<workspace>/.chatty/teams/<id>/`, then
//! `<data_dir>/chatty/teams/<id>/`, then the presets compiled into the
//! binary ([`PRESETS`]). The first directory with a `team.json` wins; a
//! malformed file there is an error, not a fall-through, so a typo never
//! silently runs the preset instead.
//!
//! `handoffs` (TD-2, AGE-693) names a JSON Schema per role, relative to the
//! team directory. Each is read and compiled when the team loads: a missing
//! file, a file that is not JSON, a schema that does not compile, or a role
//! that is not on the roster fails the load, never the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::agent_spec::{AgentSpec, load_agent_spec_from};
use crate::services::handoff::{self, HandoffContract};
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
use crate::settings::models::{ExecutionSettingsModel, ModuleSettingsModel, ProviderModel};

/// A team compiled into the binary: its `team.json`, the `SKILL.md` beside
/// it when it has one, and the handoff schemas its `handoffs` names, by the
/// same relative path the file uses.
#[derive(Clone, Copy, Debug)]
pub struct TeamPreset {
    pub id: &'static str,
    pub team_json: &'static str,
    pub skill: Option<&'static str>,
    /// `(path relative to the team directory, schema JSON)`.
    pub schemas: &'static [(&'static str, &'static str)],
}

/// The presets compiled into the binary.
///
/// Every preset team is experimental (PL-S8): none is a documented default
/// until a benchmark shows it beats a single agent. Their specs name no
/// models on purpose, so they come from the roster's default, `--model`, or a
/// spec of your own that shadows one. A preset without a `SKILL.md` has its
/// playbook in the leader's preamble, so `/agent <leader>` runs it the same
/// as `--team <id>`.
pub const PRESETS: &[TeamPreset] = &[
    TeamPreset {
        id: "crosscheck",
        team_json: include_str!("../../teams/crosscheck/team.json"),
        skill: None,
        schemas: &[(
            "schemas/judge.json",
            include_str!("../../teams/crosscheck/schemas/judge.json"),
        )],
    },
    TeamPreset {
        id: "crosscheck-data",
        team_json: include_str!("../../teams/crosscheck-data/team.json"),
        skill: None,
        schemas: &[(
            "schemas/judge.json",
            include_str!("../../teams/crosscheck-data/schemas/judge.json"),
        )],
    },
];

/// Where an experimental preset's results are written up (AGE-696).
pub const EXPERIMENTAL_NOTES: &str = "docs/research/";

/// The presets shown wherever teams are listed. An `experimental` one is
/// left out: it runs only when asked for by its exact id (AGE-696).
pub fn listed_presets() -> impl Iterator<Item = &'static TeamPreset> {
    PRESETS
        .iter()
        .filter(|preset| !TeamFile::parse(preset.team_json).is_ok_and(|file| file.experimental))
}

/// `the presets are a (A), b` for a team list in an error or help line.
fn preset_list<'a>(presets: impl Iterator<Item = &'a TeamPreset>) -> String {
    let names: Vec<String> = presets
        .map(|preset| {
            match TeamFile::parse(preset.team_json)
                .ok()
                .and_then(|file| file.name)
            {
                Some(name) => format!("{} ({name})", preset.id),
                None => preset.id.to_string(),
            }
        })
        .collect();
    if names.is_empty() {
        "no preset is listed".to_string()
    } else {
        format!("the presets are {}", names.join(", "))
    }
}

/// The relative directory a team of that id lives in under a workspace.
pub const WORKSPACE_TEAMS_DIR: &str = ".chatty/teams";

/// `team.json` as written: a thin file of agent spec names (AGE-614).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamFile {
    /// What the team is called where people pick it ("Crosscheck"); the
    /// directory name is its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Not yet shown where teams are listed (AGE-696): it runs only when
    /// asked for by its exact id, and says so when it starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub experimental: bool,
    /// The spec the leader runs as.
    pub leader: String,
    /// The roster, by spec name; it replaces `module_settings.virtual_agents`
    /// for the run.
    #[serde(default)]
    pub agents: Vec<String>,
    /// The team's verification command (`module_settings.team.verification`,
    /// AGE-406) for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// Whether each of the team's workers gets a `git worktree` of its own
    /// (ADR-0012) rather than the conversation's workspace (AGE-822). Off
    /// by default: an analysis team reads and writes where the user's files
    /// are. A coding team, whose workers edit the same files in parallel and
    /// hand back branches to merge, turns it on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub isolate: bool,
    /// The skill the leader is told to read and follow on its first turn.
    /// Served by `read_skill` from the `SKILL.md` beside this file when
    /// there is one, else from the usual skill directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The leader's turn budget for the run, ahead of the leader spec's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_turns: Option<u32>,
    /// Role → the JSON Schema its handoff must match, as a path relative to
    /// the team directory (TD-2, AGE-693). Absent is today's behaviour: a
    /// worker's answer is free text.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub handoffs: BTreeMap<String, String>,
}

impl TeamFile {
    /// Parse `team.json`. The pre-spec shape — a `leader` object, agent
    /// objects in `agents` — is refused with where its fields went, not read
    /// (no backward compatibility, PL-D2).
    pub fn parse(text: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        if value
            .get("leader")
            .is_some_and(serde_json::Value::is_object)
        {
            bail!(
                "`leader` is an object (the old team.json shape): name the leader's agent \
                 spec instead, written in .chatty/agents/<name>.toml"
            );
        }
        if value
            .get("agents")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|agents| agents.iter().any(serde_json::Value::is_object))
        {
            bail!(
                "`agents` lists an agent object (the old team.json shape): list agent spec \
                 names instead, each written in .chatty/agents/<name>.toml"
            );
        }
        Ok(serde_json::from_value(value)?)
    }
}

/// Where a team was found.
#[derive(Clone, Debug, PartialEq)]
pub enum TeamSource {
    /// A `team.json` on disk, in this directory.
    Dir(PathBuf),
    /// One of [`PRESETS`].
    Preset,
}

/// A skill served from a team directory rather than a skills directory:
/// `read_skill <name>` returns `content` ahead of any file lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamSkill {
    pub name: String,
    pub content: String,
}

/// A loaded team: the file, where it came from, the skill beside it, and
/// the specs its names resolve to.
#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    pub id: String,
    pub source: TeamSource,
    pub file: TeamFile,
    /// The `SKILL.md` beside `team.json`, when there is one.
    pub skill_content: Option<String>,
    /// `file.leader`, resolved.
    pub leader: AgentSpec,
    /// `file.agents`, resolved, in order.
    pub agents: Vec<AgentSpec>,
    /// `file.handoffs`, read and compiled: role → its contract.
    pub handoffs: BTreeMap<String, HandoffContract>,
}

impl Team {
    /// The module settings this run's broker starts from: `on_disk` with
    /// the roster and the verification command replaced by the team's.
    ///
    /// A copy, never the host's own `ModuleSettingsModel`: chatty-tui's
    /// `/modules` saves that struct back to disk on any change, and a team
    /// is declared for the run, not persisted (the AGE-382 rule).
    pub fn run_module_settings(&self, on_disk: &ModuleSettingsModel) -> ModuleSettingsModel {
        let mut settings = on_disk.clone();
        settings.virtual_agents = self.file.agents.clone();
        settings.team.verification = self.file.verification.clone();
        settings.team.isolate = self.file.isolate;
        settings
    }

    /// The names the run's broker publishes: the roster's, or the one
    /// default worker when the file declares none, as
    /// [`resolve_virtual_agents`](super::virtual_agents::resolve_virtual_agents)
    /// resolves an empty roster. A team never widens to every exposed spec.
    pub fn agent_names(&self) -> Vec<String> {
        if self.file.agents.is_empty() {
            vec![crate::tools::LOCAL_AGENT_NAME.to_string()]
        } else {
            self.file.agents.clone()
        }
    }

    /// The roster's specs for one run. With `model` (`--model`), every
    /// member runs that model, a pinned one included, so one flag moves a
    /// whole team onto one model (AGE-808); without it, each spec keeps its
    /// own `model`, or the roster's default.
    pub fn run_roster(&self, model: Option<&str>) -> Vec<AgentSpec> {
        let mut roster = self.agents.clone();
        if let Some(model) = model {
            for spec in &mut roster {
                spec.agent.model = Some(model.to_string());
            }
        }
        roster
    }

    /// The leader's turn budget for the run, when the file names one.
    /// Execution settings are the run's own copy on every host that reads a
    /// team, so this mutates in place.
    pub fn apply_turn_budget(&self, execution_settings: &mut ExecutionSettingsModel) {
        if let Some(turns) = self.file.max_agent_turns {
            execution_settings.max_agent_turns = turns;
        }
    }

    /// What the leader's `invoke_agent` records this run's handoffs in:
    /// the invalid count per role and the read rules' `handoff_misread`
    /// tag (TD-1's scorecard). `None` for a team without `handoffs`.
    pub fn handoff_ledger(&self) -> Option<handoff::HandoffLedger> {
        (!self.handoffs.is_empty()).then(|| handoff::HandoffLedger::new(self.handoffs.values()))
    }

    /// The skill `read_skill` should serve from this team, when the file
    /// names one and its `SKILL.md` is beside it.
    pub fn skill(&self) -> Option<TeamSkill> {
        Some(TeamSkill {
            name: self.file.skill.clone()?,
            content: self.skill_content.clone()?,
        })
    }

    /// What the leader's first turn starts with: the instruction to read the
    /// team's skill and follow it, and — since a `coordinator` leader has
    /// no shell and can only delegate the check — the verification command
    /// it has to hand a worker. `None` when the file names no skill.
    pub fn first_turn_instruction(&self) -> Option<String> {
        let skill = self.file.skill.as_deref()?;
        let mut text = format!("read_skill {skill} and follow it.");
        if let Some(command) = self.file.verification.as_deref() {
            text.push_str(&format!(" The team's verification command is: `{command}`"));
        }
        Some(text)
    }

    /// The one line a run of an `experimental` team prints when it starts
    /// (AGE-696); `None` for any other team.
    pub fn experimental_notice(&self) -> Option<String> {
        self.file.experimental.then(|| {
            format!(
                "{} is experimental: see {EXPERIMENTAL_NOTES}",
                self.file.name.as_deref().unwrap_or(&self.id)
            )
        })
    }
}

/// The verification command declared by whichever team — a preset compiled
/// into the binary, or a `<workspace>/.chatty/teams/<id>/team.json` — has
/// `name` as its leader or on its roster (AGE-763): `/agent <name>` runs
/// with that team's own tests, exactly as `--team <id>` does, rather than
/// module settings' own `team.verification`. A workspace team beats a
/// preset of the same id, as [`load_team`] shadows it; reads only the team
/// file, never the specs it names, so a spec that does not load never
/// blocks this lookup. `None` when no team claims `name`, or the team that
/// does declares no verification command.
pub fn verification_for_member(name: &str, workspace: Option<&Path>) -> Option<String> {
    team_file_for_member(name, workspace).and_then(|file| file.verification)
}

/// Whether the team that claims `name` — found as
/// [`verification_for_member`] finds it — isolates its workers in
/// worktrees (AGE-822). `None` when no team claims `name`.
pub fn isolate_for_member(name: &str, workspace: Option<&Path>) -> Option<bool> {
    team_file_for_member(name, workspace).map(|file| file.isolate)
}

/// The `team.json` of the team that has `name` as its leader or on its
/// roster: a workspace team first, then a preset.
fn team_file_for_member(name: &str, workspace: Option<&Path>) -> Option<TeamFile> {
    if let Some(root) = workspace {
        let dir = root.join(WORKSPACE_TEAMS_DIR);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(path.join("team.json")) else {
                    continue;
                };
                let Ok(file) = TeamFile::parse(&text) else {
                    continue;
                };
                if file.leader == name || file.agents.iter().any(|agent| agent == name) {
                    return Some(file);
                }
            }
        }
    }
    PRESETS
        .iter()
        .map(|preset| TeamFile::parse(preset.team_json).expect("a preset team.json parses"))
        .find(|file| file.leader == name || file.agents.iter().any(|agent| agent == name))
}

/// Load the team `id` from the workspace, the data directory, or the
/// presets, in that order, and the specs it names from the same places.
///
/// `workspace` is the run's workspace root and `data_dir` the platform data
/// directory (`dirs::data_dir()`), each `None` when unknown.
pub fn load_team(id: &str, workspace: Option<&Path>, data_dir: Option<&Path>) -> Result<Team> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
        bail!("'{id}' is not a team id (a directory name under teams/)");
    }
    let candidates = [
        workspace.map(|root| root.join(WORKSPACE_TEAMS_DIR).join(id)),
        data_dir.map(|root| root.join("chatty").join("teams").join(id)),
    ];
    let mut found = None;
    for dir in candidates.iter().flatten() {
        let json = dir.join("team.json");
        if !json.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&json)
            .with_context(|| format!("failed to read {}", json.display()))?;
        let file = TeamFile::parse(&text)
            .with_context(|| format!("{} is not a team file", json.display()))?;
        let skill_content = std::fs::read_to_string(dir.join("SKILL.md"))
            .ok()
            .filter(|s| !s.trim().is_empty());
        found = Some((TeamSource::Dir(dir.clone()), file, skill_content));
        break;
    }
    if found.is_none()
        && let Some(preset) = PRESETS.iter().find(|preset| preset.id == id)
    {
        let file = TeamFile::parse(preset.team_json).expect("a preset team.json parses");
        found = Some((TeamSource::Preset, file, preset.skill.map(str::to_string)));
    }
    let Some((source, file, skill_content)) = found else {
        bail!(
            "no team '{id}': looked in {}, and {}",
            candidates
                .iter()
                .flatten()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            preset_list(listed_presets())
        );
    };
    let spec = |name: &str| {
        load_agent_spec_from(name, workspace, data_dir)
            .map(|loaded| loaded.spec)
            .with_context(|| format!("team '{id}' names agent spec '{name}'"))
    };
    let today = chrono::Local::now().date_naive();
    let mut leader = spec(&file.leader)?;
    fill_today(&mut leader, today);
    let agents = file
        .agents
        .iter()
        .map(|name| {
            let mut member = spec(name)?;
            fill_today(&mut member, today);
            Ok(member)
        })
        .collect::<Result<Vec<_>>>()?;
    let handoffs = load_handoffs(id, &file, &source)
        .with_context(|| format!("team '{id}' names a handoff schema that does not load"))?;
    Ok(Team {
        id: id.to_string(),
        source,
        file,
        skill_content,
        leader,
        agents,
        handoffs,
    })
}

/// What a team member's preamble writes where today's date goes. A model
/// asked for "today" makes one up (AGE-808: an ADR came back dated
/// 2026-07-24), so [`load_team`] replaces the token with the local date,
/// `YYYY-MM-DD`, before anyone sees the preamble.
pub const TODAY_TOKEN: &str = "{{today}}";

/// Replace every [`TODAY_TOKEN`] in `spec`'s preamble with `today`.
pub fn fill_today(spec: &mut AgentSpec, today: chrono::NaiveDate) {
    if let Some(preamble) = spec.agent.preamble.as_mut()
        && preamble.contains(TODAY_TOKEN)
    {
        *preamble = preamble.replace(TODAY_TOKEN, &today.format("%Y-%m-%d").to_string());
    }
}

/// Refuse to start agents whose pinned model cannot run, naming each agent,
/// its pin, the provider it needs when that is known, and the override that
/// fixes it (AGE-808), rather than letting each worker fail on its own the
/// first time it is delegated to.
///
/// A pinned model that resolves among `models` needs its own provider
/// configured (an API key; Azure's endpoint and key or Entra ID; Ollama
/// always counts). A pin that resolves to nothing needs OpenRouter when it
/// is an OpenRouter id — `vendor/model`, the form OpenRouter's catalogue and
/// Chatty's sync of it use — and OpenRouter is not configured; otherwise it
/// names no model the user has. A spec without a model passes. The caller
/// leaves out a spec whose model came from `--model`: a mistyped flag gets
/// the "model not found" error that lists the models there are.
pub fn check_model_providers<'a>(
    specs: impl IntoIterator<Item = &'a AgentSpec>,
    models: &[ModelConfig],
    providers: &[ProviderConfig],
) -> Result<()> {
    let mut store = ProviderModel::new();
    store.replace_all(providers.to_vec());
    let configured = |kind: &ProviderType| {
        store
            .configured_providers()
            .any(|provider| &provider.provider_type == kind)
    };
    let mut no_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut no_model: Vec<String> = Vec::new();
    for spec in specs {
        let Some(query) = spec.agent.model.as_deref() else {
            continue;
        };
        let pin = format!("{} (model = \"{query}\")", spec.agent.name);
        match resolve_model_query(models, Some(query)) {
            Some(model) if !configured(&model.provider_type) => no_provider
                .entry(model.provider_type.display_name().to_string())
                .or_default()
                .push(pin),
            Some(_) => {}
            None if is_openrouter_id(query) && !configured(&ProviderType::OpenRouter) => {
                no_provider
                    .entry(ProviderType::OpenRouter.display_name().to_string())
                    .or_default()
                    .push(pin)
            }
            None => no_model.push(pin),
        }
    }
    if no_provider.is_empty() && no_model.is_empty() {
        return Ok(());
    }
    let mut lines: Vec<String> = no_provider
        .into_iter()
        .map(|(provider, pins)| {
            format!(
                "{provider} is not configured, and these agents pin a model it serves: {}. \
                 Add your {provider} credentials in Settings → Providers, or run them on a model \
                 you have (below).",
                pins.join(", ")
            )
        })
        .collect();
    if !no_model.is_empty() {
        let yours = models
            .iter()
            .map(|model| model.name.as_str())
            .collect::<Vec<_>>();
        lines.push(format!(
            "No model you have matches these pins: {}. Your models: {}.",
            no_model.join(", "),
            if yours.is_empty() {
                "none".to_string()
            } else {
                yours.join(", ")
            }
        ));
    }
    lines.push(
        "To run on a model you have: `--model <model>` runs every agent of a `--team` run on \
         it; or override one agent with <workspace>/.chatty/agents/<agent>.toml, a copy of its \
         built-in spec with the `model` line set to one of your models, or removed to use your \
         default model."
            .to_string(),
    );
    bail!("{}", lines.join("\n"))
}

/// Whether `query` has the shape of an OpenRouter model id: one `vendor/`
/// prefix (no dots in it, so not a registry host like `hf.co/…`) and a model.
fn is_openrouter_id(query: &str) -> bool {
    match query.split_once('/') {
        Some((vendor, model)) => {
            !vendor.is_empty()
                && !vendor.contains('.')
                && !model.is_empty()
                && !model.contains('/')
                && !query.contains(char::is_whitespace)
        }
        None => false,
    }
}

/// Read and compile every schema `file.handoffs` names, from the team's
/// directory or, for a preset, from the schemas compiled in beside it. A
/// role must be on the roster (the leader hands off to nobody), and a
/// schema's `x-must-be-read` may only name roles that have a schema of
/// their own.
fn load_handoffs(
    id: &str,
    file: &TeamFile,
    source: &TeamSource,
) -> Result<BTreeMap<String, HandoffContract>> {
    if file.handoffs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut contracts = BTreeMap::new();
    for (role, relative) in &file.handoffs {
        if !file.agents.contains(role) {
            bail!("`handoffs` names '{role}', which is not in `agents`");
        }
        let (path, text) = match source {
            TeamSource::Dir(dir) => {
                let path = dir.join(relative);
                let text = std::fs::read_to_string(&path).with_context(|| {
                    format!("failed to read {role}'s schema {}", path.display())
                })?;
                (path, text)
            }
            TeamSource::Preset => {
                let text = PRESETS
                    .iter()
                    .find(|preset| preset.id == id)
                    .and_then(|preset| {
                        preset
                            .schemas
                            .iter()
                            .find(|(path, _)| *path == relative.as_str())
                    })
                    .map(|(_, text)| text.to_string())
                    .with_context(|| {
                        format!("preset '{id}' does not compile in {role}'s schema {relative}")
                    })?;
                (PathBuf::from(relative), text)
            }
        };
        let schema: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("{role}'s schema {} is not JSON", path.display()))?;
        handoff::compile(&schema).map_err(|e| {
            anyhow::anyhow!(
                "{role}'s schema {} is not a valid JSON Schema: {e}",
                path.display()
            )
        })?;
        for earlier in handoff::read_rules(&schema)
            .map_err(|e| anyhow::anyhow!("{role}'s schema {}: {e}", path.display()))?
            .keys()
        {
            if !file.handoffs.contains_key(earlier) {
                bail!(
                    "{role}'s schema {} must read '{earlier}', which has no handoff schema",
                    path.display()
                );
            }
        }
        contracts.insert(
            role.clone(),
            HandoffContract {
                role: role.clone(),
                schema: chatty_fabric::wire::Opaque::from_value(&schema)
                    .with_context(|| format!("{role}'s schema {}", path.display()))?,
            },
        );
    }
    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::WORKSPACE_AGENTS_DIR;

    fn write_team(dir: &Path, id: &str, json: &str, skill: Option<&str>) -> PathBuf {
        let team_dir = dir.join(id);
        std::fs::create_dir_all(&team_dir).unwrap();
        std::fs::write(team_dir.join("team.json"), json).unwrap();
        if let Some(skill) = skill {
            std::fs::write(team_dir.join("SKILL.md"), skill).unwrap();
        }
        team_dir
    }

    const CROSSCHECK_ROSTER: [&str; 5] = [
        "crosscheck-solver-direct",
        "crosscheck-solver-plan",
        "crosscheck-solver-verify",
        "crosscheck-judge",
        "crosscheck-writer",
    ];

    fn write_spec(root: &Path, name: &str, body: &str) {
        let dir = root.join(WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.toml")),
            format!("[agent]\nname = \"{name}\"\n{body}"),
        )
        .unwrap();
    }

    /// AGE-752: every preset team loads; its leader delegates to exactly its
    /// roster, may be called by name (`/agent <leader>`), says it is
    /// experimental (PL-S8), and its workers are workers, not sub-leaders.
    #[test]
    fn every_preset_team_loads_and_its_leader_delegates_to_its_roster() {
        for preset in PRESETS {
            let id = preset.id;
            let team = load_team(id, None, None).expect("the preset loads");
            assert_eq!(team.source, TeamSource::Preset);
            assert_eq!(
                team.leader.swarm.delegates_to, team.file.agents,
                "{id}: the leader calls its roster and nobody else"
            );
            assert!(team.leader.swarm.exposed, "{id}: /agent reaches the leader");
            assert!(
                team.leader
                    .agent
                    .description
                    .as_deref()
                    .is_some_and(|d| d.starts_with("Experimental team lead")),
                "{id}: {:?}",
                team.leader.agent.description
            );
            for spec in &team.agents {
                assert!(spec.swarm.exposed, "{id}: {} is callable", spec.agent.name);
                assert!(
                    spec.swarm.delegates_to.is_empty(),
                    "{id}: {} is a worker, not a sub-leader",
                    spec.agent.name
                );
            }
        }
        let crosscheck = load_team("crosscheck", None, None).unwrap();
        assert_eq!(crosscheck.file.leader, "crosscheck-lead");
        assert_eq!(crosscheck.file.agents, CROSSCHECK_ROSTER);
        assert!(crosscheck.skill().is_none(), "the playbook is the preamble");
        let data = load_team("crosscheck-data", None, None).unwrap();
        assert_eq!(data.file.leader, "crosscheck-data-lead");
        assert_eq!(
            data.file.agents,
            [
                "crosscheck-data-direct",
                "crosscheck-data-plan",
                "crosscheck-data-verify",
                "crosscheck-judge",
                "crosscheck-writer"
            ]
        );
        assert!(data.skill().is_none(), "the playbook is the preamble");
    }

    /// Selecting a team (`--team <id>`) puts exactly its specs on the run's
    /// roster, and its leader, the root, reaches every one of them. The
    /// leader is on the default roster too; its internal members are not.
    #[test]
    fn selecting_a_team_adds_its_presets() {
        let default_roster = crate::agent_spec::roster_names_from(&[], None, None);
        for preset in PRESETS {
            let id = preset.id;
            let team = load_team(id, None, None).expect("the preset loads");
            for name in team.file.agents.iter().chain([&team.file.leader]) {
                let internal = name != &team.file.leader
                    && load_team(id, None, None)
                        .unwrap()
                        .agents
                        .iter()
                        .any(|spec| &spec.agent.name == name && spec.swarm.callers.is_some());
                assert_eq!(
                    default_roster.contains(name),
                    !internal,
                    "{id}: {name} is on the default roster unless team-internal"
                );
            }
            assert_eq!(team.agent_names(), team.file.agents, "{id}");
            let settings = team.run_module_settings(&ModuleSettingsModel::default());
            let mut roster =
                crate::agent_spec::load_roster_from(&settings.virtual_agents, None, None)
                    .expect("the team's roster loads");
            // `load_team` dates its members' preambles (AGE-808).
            let today = chrono::Local::now().date_naive();
            roster.iter_mut().for_each(|spec| fill_today(spec, today));
            assert_eq!(roster, team.agents, "{id}: the run serves the team");
        }
    }

    /// AGE-853: the crosscheck leader is offered `best_of` over its three
    /// solvers and its judge, and the judge does not think.
    #[test]
    fn the_crosscheck_preset_names_its_solvers_and_a_judge_that_does_not_think() {
        let team = load_team("crosscheck", None, None).expect("the preset loads");
        assert_eq!(team.file.leader, "crosscheck-lead");
        let best_of = team.leader.swarm.best_of.as_ref().expect("swarm.best_of");
        assert_eq!(
            best_of.solvers,
            [
                "crosscheck-solver-direct",
                "crosscheck-solver-plan",
                "crosscheck-solver-verify"
            ]
        );
        assert_eq!(best_of.judge, "crosscheck-judge");
        let judge = team
            .agents
            .iter()
            .find(|a| a.agent.name == "crosscheck-judge")
            .expect("the judge is on the roster");
        assert_eq!(judge.agent.think, Some(false));
        assert!(team.handoffs.contains_key("crosscheck-judge"));
    }

    /// A handoff schema is what turns a reply cut off mid-sentence into an
    /// invalid handoff (and its one re-prompt) instead of a lost answer: the
    /// crosscheck judge's names a candidate by number, with its reason.
    #[test]
    fn the_crosscheck_judge_handoff_rejects_a_truncated_reply() {
        for id in ["crosscheck", "crosscheck-data"] {
            let team = load_team(id, None, None).unwrap();
            assert!(team.handoff_ledger().is_some(), "{id}");
            let judge = &team.handoffs["crosscheck-judge"];
            for reply in [
                "The",
                "```json\n{\"choice\": 2}\n```",
                "```json\n{\"choice\": 2, \"reason\": \"\"}\n```",
                "```json\n{\"choice\": 0, \"reason\": \"y\"}\n```",
                "```json\n{\"choice\": 10, \"reason\": \"y\"}\n```",
                "```json\n{\"choice\": 2, \"reason\": \"y\", \"answer\": \"x\"}\n```",
            ] {
                assert!(
                    matches!(
                        handoff::check(judge, reply),
                        handoff::HandoffOutcome::Invalid { .. }
                    ),
                    "{id}: the judge must reject {reply:?}"
                );
            }
            assert_eq!(
                handoff::check(
                    judge,
                    "Done.\n```json\n{\"choice\": 2, \"reason\": \"Only 2 checked the units.\"}\n```"
                ),
                handoff::HandoffOutcome::Valid(serde_json::json!({
                    "choice": 2, "reason": "Only 2 checked the units."
                }))
            );
        }
    }

    const STRONGEST_CLAUDE: &str = "anthropic/claude-opus-5";
    const CHEAPER_CLAUDE: &str = "anthropic/claude-sonnet-5";

    /// The models the OpenRouter sync makes of the two pinned ids.
    fn synced_claude_models() -> Vec<ModelConfig> {
        [
            (STRONGEST_CLAUDE, "Claude Opus 5"),
            (CHEAPER_CLAUDE, "Claude Sonnet 5"),
        ]
        .into_iter()
        .map(|(id, name)| {
            ModelConfig::new(
                id.replace('/', "-"),
                name.to_string(),
                ProviderType::OpenRouter,
                id.to_string(),
            )
        })
        .collect()
    }

    fn openrouter(key: Option<&str>) -> ProviderConfig {
        let provider = ProviderConfig::new("OpenRouter".to_string(), ProviderType::OpenRouter);
        match key {
            Some(key) => provider.with_api_key(key.to_string()),
            None => provider,
        }
    }

    fn team_specs(team: &Team) -> Vec<&AgentSpec> {
        std::iter::once(&team.leader).chain(&team.agents).collect()
    }

    /// A workspace team `pinned` whose specs pin hosted models: the
    /// strongest Claude for the lead and the worker, a cheaper one for the
    /// checker. The worker's preamble carries the date token.
    fn write_pinned_team(workspace: &Path) {
        write_spec(
            workspace,
            "pin-lead",
            &format!(
                "model = \"{STRONGEST_CLAUDE}\"\n[swarm]\ndelegates_to = [\"pin-worker\", \"pin-checker\"]\n"
            ),
        );
        write_spec(
            workspace,
            "pin-worker",
            &format!(
                "model = \"{STRONGEST_CLAUDE}\"\npreamble = \"decision-date: {TODAY_TOKEN}\"\n"
            ),
        );
        write_spec(
            workspace,
            "pin-checker",
            &format!("model = \"{CHEAPER_CLAUDE}\"\n"),
        );
        write_team(
            &workspace.join(WORKSPACE_TEAMS_DIR),
            "pinned",
            r#"{"leader":"pin-lead","agents":["pin-worker","pin-checker"]}"#,
            None,
        );
    }

    /// Pinned models resolve among the models the OpenRouter sync makes,
    /// which is what `spec.validate` checks, and `load_team` dates a
    /// member's preamble.
    #[test]
    fn a_teams_pinned_models_resolve_and_its_preambles_are_dated() {
        let workspace = tempfile::tempdir().unwrap();
        write_pinned_team(workspace.path());
        let team = load_team("pinned", Some(workspace.path()), None).unwrap();
        let models = synced_claude_models();
        for member in team_specs(&team) {
            let name = &member.agent.name;
            let expected = if name == "pin-checker" {
                CHEAPER_CLAUDE
            } else {
                STRONGEST_CLAUDE
            };
            assert_eq!(member.agent.model.as_deref(), Some(expected), "{name}");
            member
                .validate(Some(&models))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let resolved = resolve_model_query(&models, member.agent.model.as_deref()).unwrap();
            assert_eq!(resolved.model_identifier, expected);
        }
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d");
        let worker = team
            .agents
            .iter()
            .find(|a| a.agent.name == "pin-worker")
            .unwrap();
        assert_eq!(
            worker.agent.preamble.as_deref(),
            Some(format!("decision-date: {today}").as_str())
        );
    }

    /// AGE-808: without the provider a pinned model needs, the team fails
    /// before anything runs, naming the provider and the agents; with it, or
    /// with every spec shadowed by one without a pin, it passes.
    #[test]
    fn a_pinned_model_without_its_provider_names_the_provider() {
        let workspace = tempfile::tempdir().unwrap();
        write_pinned_team(workspace.path());
        let team = load_team("pinned", Some(workspace.path()), None).unwrap();
        let text = |models: &[ModelConfig], providers: &[ProviderConfig]| {
            format!(
                "{:#}",
                check_model_providers(team_specs(&team), models, providers).unwrap_err()
            )
        };

        // No OpenRouter at all, so the sync never added the models.
        let error = text(&[], &[]);
        assert!(error.contains("OpenRouter is not configured"), "{error}");
        assert!(
            error.contains("pin-lead (model = \"anthropic/claude-opus-5\")"),
            "{error}"
        );
        assert!(
            error.contains("pin-checker (model = \"anthropic/claude-sonnet-5\")"),
            "{error}"
        );
        assert!(error.contains("Settings → Providers"), "{error}");
        assert!(
            error.contains("`--model <model>` runs every agent"),
            "{error}"
        );
        assert!(
            error.contains("<workspace>/.chatty/agents/<agent>.toml"),
            "{error}"
        );

        // The models are there, but the provider has no key.
        let error = text(&synced_claude_models(), &[openrouter(None)]);
        assert!(error.contains("OpenRouter is not configured"), "{error}");
        let error = text(&synced_claude_models(), &[openrouter(Some("  "))]);
        assert!(error.contains("OpenRouter is not configured"), "{error}");

        check_model_providers(
            team_specs(&team),
            &synced_claude_models(),
            &[openrouter(Some("sk-or-test"))],
        )
        .expect("with a key the team runs");

        // OpenRouter is there but the pin is not among the models (an Azure
        // user's roster, say): the error names the agent, the pin, the
        // models there are, and the override.
        let mut azure = ModelConfig::new(
            "gpt-56".to_string(),
            "GPT-5.6 (Azure)".to_string(),
            ProviderType::AzureOpenAI,
            "gpt-5.6-deployment".to_string(),
        );
        azure.supports_temperature = false;
        let error = text(
            std::slice::from_ref(&azure),
            &[openrouter(Some("sk-or-test"))],
        );
        assert!(
            error.contains("No model you have matches these pins"),
            "{error}"
        );
        assert!(
            error.contains("pin-worker (model = \"anthropic/claude-opus-5\")"),
            "{error}"
        );
        assert!(error.contains("Your models: GPT-5.6 (Azure)."), "{error}");
        assert!(!error.contains("is not configured"), "{error}");

        // A pin in no provider's id form is reported the same way.
        let mut local = AgentSpec::named("local");
        local.agent.model = Some("qwen3:4b".to_string());
        let error = format!(
            "{:#}",
            check_model_providers([&local], &[], &[]).unwrap_err()
        );
        assert!(error.contains("local (model = \"qwen3:4b\")"), "{error}");
        assert!(error.contains("Your models: none."), "{error}");
        assert!(!is_openrouter_id("hf.co/bartowski/qwen"));
        assert!(is_openrouter_id("deepseek/deepseek-r1:free"));

        // Every spec rewritten without a pin: the roster's default runs it.
        let dir = workspace.path().join(WORKSPACE_AGENTS_DIR);
        for member in team_specs(&team) {
            let mut shadow = member.clone();
            shadow.agent.model = None;
            std::fs::write(
                dir.join(format!("{}.toml", shadow.agent.name)),
                shadow.to_toml().unwrap(),
            )
            .unwrap();
        }
        let shadowed = load_team("pinned", Some(workspace.path()), None).unwrap();
        check_model_providers(team_specs(&shadowed), &[], &[])
            .expect("a shadowed team needs no hosted provider");
    }

    /// AGE-808: a preamble's `{{today}}` becomes the date; a spec without
    /// the token is left as it was.
    #[test]
    fn a_members_preamble_gets_todays_date() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let mut spec = AgentSpec::from_toml(
            "[agent]\nname = \"p\"\npreamble = \"decision-date: {{today}}; again {{today}}\"\n",
        )
        .unwrap();
        fill_today(&mut spec, date);
        assert_eq!(
            spec.agent.preamble.as_deref(),
            Some("decision-date: 2026-10-02; again 2026-10-02")
        );
        let mut plain = AgentSpec::from_toml("[agent]\nname = \"q\"\n").unwrap();
        fill_today(&mut plain, date);
        assert_eq!(plain.agent.preamble, None);
    }

    /// AGE-808: `--model` on a team run is the one-flag way onto a single
    /// model: every member runs it, the pins included; without it the pins
    /// stand.
    #[test]
    fn model_for_the_run_replaces_every_members_pin() {
        let workspace = tempfile::tempdir().unwrap();
        write_pinned_team(workspace.path());
        let team = load_team("pinned", Some(workspace.path()), None).unwrap();
        let pinned = team.run_roster(None);
        assert_eq!(pinned, team.agents);
        assert!(pinned.iter().all(|spec| spec.agent.model.is_some()));
        let one = team.run_roster(Some("gpt-5.6-deployment"));
        assert_eq!(one.len(), team.agents.len());
        for spec in &one {
            assert_eq!(
                spec.agent.model.as_deref(),
                Some("gpt-5.6-deployment"),
                "{}",
                spec.agent.name
            );
        }
        let data = load_team("crosscheck-data", None, None).unwrap();
        assert!(
            data.run_roster(None)
                .iter()
                .all(|s| s.agent.model.is_none())
        );
        assert!(
            data.run_roster(Some("m"))
                .iter()
                .all(|s| s.agent.model.as_deref() == Some("m"))
        );
    }

    /// A preset whose `handoffs` names a schema it does not compile in fails
    /// to load, as a missing file in a team directory does.
    #[test]
    fn every_preset_compiles_in_the_schemas_it_names() {
        for preset in PRESETS {
            let file = TeamFile::parse(preset.team_json).unwrap();
            for relative in file.handoffs.values() {
                assert!(
                    preset.schemas.iter().any(|(path, _)| path == relative),
                    "preset {} names {relative} but does not compile it in",
                    preset.id
                );
            }
            load_team(preset.id, None, None).unwrap();
        }
    }

    /// A team file in the workspace beats the preset of the same id, the
    /// data directory sits between the two, and the specs it names are
    /// looked up the same way — a workspace spec shadows the preset one.
    #[test]
    fn a_workspace_team_overrides_the_preset_and_the_data_dir() {
        let workspace = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let ws_teams = workspace.path().join(WORKSPACE_TEAMS_DIR);
        let data_teams = data.path().join("chatty").join("teams");

        std::fs::create_dir_all(data.path().join("chatty").join("agents")).unwrap();
        std::fs::write(
            data.path()
                .join("chatty")
                .join("agents")
                .join("data-coder.toml"),
            "[agent]\nname = \"data-coder\"\n",
        )
        .unwrap();
        write_team(
            &data_teams,
            "crosscheck",
            r#"{"leader":"crosscheck-lead","agents":["data-coder"],"skill":"crosscheck"}"#,
            Some("# data skill"),
        );
        let team = load_team("crosscheck", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(team.source, TeamSource::Dir(data_teams.join("crosscheck")));
        assert_eq!(team.agents[0].agent.name, "data-coder");
        assert_eq!(team.skill().unwrap().content, "# data skill");

        write_spec(
            workspace.path(),
            "ws-coder",
            "[tools]\nprofile = \"coder\"\n",
        );
        write_spec(
            workspace.path(),
            "crosscheck-lead",
            "model = \"big\"\npreamble = \"lead\"\n[tools]\nprofile = \"coordinator\"\n",
        );
        let ws_dir = write_team(
            &ws_teams,
            "crosscheck",
            r#"{
              "leader": "crosscheck-lead",
              "agents": ["ws-coder"],
              "verification": "cargo test",
              "skill": "crosscheck",
              "max_agent_turns": 7
            }"#,
            None,
        );
        let team = load_team("crosscheck", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(team.source, TeamSource::Dir(ws_dir));
        assert_eq!(team.leader.agent.model.as_deref(), Some("big"));
        assert_eq!(team.agents[0].agent.name, "ws-coder");
        assert_eq!(team.file.verification.as_deref(), Some("cargo test"));
        assert_eq!(team.file.max_agent_turns, Some(7));
        assert!(
            team.skill().is_none(),
            "no SKILL.md beside it: read_skill falls back to the skill dirs"
        );
        assert_eq!(
            team.first_turn_instruction().as_deref(),
            Some(
                "read_skill crosscheck and follow it. \
                 The team's verification command is: `cargo test`"
            )
        );
    }

    /// The team is declared on the run's settings and nowhere else: the
    /// run's module settings are a copy with the roster and verification
    /// replaced (the on-disk struct is untouched), and the turn budget
    /// replaces execution settings' only when the file names one.
    #[test]
    fn the_run_settings_carry_the_roster_verification_and_turn_budget() {
        let mut on_disk = ModuleSettingsModel {
            default_endpoint_budget: 7,
            virtual_agents: vec!["stale".to_string()],
            ..Default::default()
        };
        on_disk.team.verification = Some("old".to_string());
        let mut execution_settings = ExecutionSettingsModel::default();
        let before = execution_settings.max_agent_turns;

        let mut team = load_team("crosscheck", None, None).unwrap();
        team.file.max_agent_turns = None;
        let run = team.run_module_settings(&on_disk);
        assert_eq!(run.virtual_agents, CROSSCHECK_ROSTER);
        assert_eq!(team.agent_names(), CROSSCHECK_ROSTER);
        assert!(run.team.verification.is_none());
        assert_eq!(
            run.default_endpoint_budget, 7,
            "everything else is the on-disk value"
        );
        assert_eq!(on_disk.virtual_agents, ["stale"]);
        assert_eq!(on_disk.team.verification.as_deref(), Some("old"));
        team.apply_turn_budget(&mut execution_settings);
        assert_eq!(execution_settings.max_agent_turns, before);

        team.file.max_agent_turns = Some(50);
        team.file.verification = Some("make test".to_string());
        team.apply_turn_budget(&mut execution_settings);
        assert_eq!(execution_settings.max_agent_turns, 50);
        assert_eq!(
            team.run_module_settings(&on_disk)
                .team
                .verification
                .as_deref(),
            Some("make test")
        );

        team.file.agents.clear();
        assert_eq!(team.agent_names(), [crate::tools::LOCAL_AGENT_NAME]);
    }

    #[test]
    fn a_malformed_team_file_is_an_error_not_a_fall_through() {
        let workspace = tempfile::tempdir().unwrap();
        write_team(
            &workspace.path().join(WORKSPACE_TEAMS_DIR),
            "crosscheck",
            "{ not json",
            None,
        );
        let err = load_team("crosscheck", Some(workspace.path()), None).unwrap_err();
        assert!(err.to_string().contains("not a team file"), "{err:#}");
    }

    /// No backward compatibility (PL-D2): the pre-spec shape fails to load,
    /// naming the field and where agents are declared now.
    #[test]
    fn an_old_shape_team_file_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        for (json, field) in [
            (
                r#"{"leader":{"profile":"coordinator"},"agents":["local-coder"]}"#,
                "`leader`",
            ),
            (
                r#"{"leader":"crosscheck-lead","agents":[{"name":"local-coder","tools":"coder"}]}"#,
                "`agents`",
            ),
        ] {
            write_team(
                &workspace.path().join(WORKSPACE_TEAMS_DIR),
                "old",
                json,
                None,
            );
            let err = format!(
                "{:#}",
                load_team("old", Some(workspace.path()), None).unwrap_err()
            );
            assert!(err.contains(field), "{err}");
            assert!(err.contains(".chatty/agents/"), "{err}");
        }
    }

    /// TD-2: a team whose `handoffs` names a schema that is missing, not
    /// JSON, not a JSON Schema, for a role off the roster, or reading a
    /// role without a schema fails to load, saying which; a good one loads
    /// with each role's schema compiled in.
    #[test]
    fn team_with_bad_schema_fails_to_load() {
        let workspace = tempfile::tempdir().unwrap();
        let teams = workspace.path().join(WORKSPACE_TEAMS_DIR);
        let team_json = |handoffs: &str| {
            format!(
                r#"{{"leader":"crosscheck-lead","agents":["researcher","reviewer"],"handoffs":{handoffs}}}"#
            )
        };
        let schemas = teams.join("t").join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        std::fs::write(schemas.join("not-json.json"), "{ nope").unwrap();
        std::fs::write(schemas.join("bad.json"), r#"{"type": 5}"#).unwrap();
        std::fs::write(
            schemas.join("change.json"),
            r#"{"type":"object","required":["files_changed"]}"#,
        )
        .unwrap();
        std::fs::write(
            schemas.join("review.json"),
            r#"{"type":"object","x-must-be-read":{"researcher":["files_changed"]}}"#,
        )
        .unwrap();

        for (handoffs, says) in [
            (r#"{"researcher":"schemas/missing.json"}"#, "failed to read"),
            (r#"{"researcher":"schemas/not-json.json"}"#, "is not JSON"),
            (
                r#"{"researcher":"schemas/bad.json"}"#,
                "not a valid JSON Schema",
            ),
            (r#"{"nobody":"schemas/change.json"}"#, "not in `agents`"),
            (
                r#"{"reviewer":"schemas/review.json"}"#,
                "has no handoff schema",
            ),
        ] {
            write_team(&teams, "t", &team_json(handoffs), None);
            let err = format!(
                "{:#}",
                load_team("t", Some(workspace.path()), None).unwrap_err()
            );
            assert!(err.contains("handoff schema"), "{err}");
            assert!(err.contains(says), "{handoffs}: {err}");
        }

        write_team(
            &teams,
            "t",
            &team_json(r#"{"researcher":"schemas/change.json","reviewer":"schemas/review.json"}"#),
            None,
        );
        let team = load_team("t", Some(workspace.path()), None).unwrap();
        assert_eq!(team.handoffs["researcher"].role, "researcher");
        assert_eq!(
            team.handoffs["researcher"].schema.to_value().unwrap()["required"][0],
            "files_changed"
        );
        assert!(team.handoff_ledger().is_some());
        write_team(
            &teams,
            "plain",
            r#"{"leader":"crosscheck-lead","agents":["researcher","reviewer"]}"#,
            None,
        );
        let plain = load_team("plain", Some(workspace.path()), None).unwrap();
        assert!(plain.handoffs.is_empty() && plain.handoff_ledger().is_none());
    }

    #[test]
    fn a_team_naming_a_missing_spec_says_which() {
        let workspace = tempfile::tempdir().unwrap();
        write_team(
            &workspace.path().join(WORKSPACE_TEAMS_DIR),
            "t",
            r#"{"leader":"crosscheck-lead","agents":["nobody"]}"#,
            None,
        );
        let err = format!(
            "{:#}",
            load_team("t", Some(workspace.path()), None).unwrap_err()
        );
        assert!(err.contains("'nobody'"), "{err}");
    }

    /// The error names where it looked and the listed presets; an
    /// experimental preset is not among them (AGE-696).
    #[test]
    fn an_unknown_team_lists_where_it_looked_and_the_presets() {
        let workspace = tempfile::tempdir().unwrap();
        let err = load_team("nope", Some(workspace.path()), None).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(".chatty/teams/nope"), "{text}");
        for preset in listed_presets() {
            assert!(text.contains(preset.id), "{text}");
        }
        assert!(!text.contains("crosscheck"), "{text}");
        assert!(load_team("../x", None, None).is_err());
        assert!(load_team("", None, None).is_err());
    }

    /// AGE-696: `experimental` lists a preset nowhere, and is written only
    /// when set.
    #[test]
    fn an_experimental_team_is_not_listed() {
        let listed = |json: &'static str| {
            let preset = TeamPreset {
                id: "t",
                team_json: json,
                skill: None,
                schemas: &[],
            };
            preset_list([preset].iter())
        };
        assert_eq!(
            listed(r#"{"name":"T","leader":"l"}"#),
            "the presets are t (T)"
        );
        for preset in PRESETS {
            let file = TeamFile::parse(preset.team_json).unwrap();
            assert_eq!(
                listed_presets().any(|p| p.id == preset.id),
                !file.experimental,
                "{}",
                preset.id
            );
        }
        let plain = TeamFile::parse(r#"{"leader":"l"}"#).unwrap();
        assert!(!plain.experimental);
        assert!(
            !serde_json::to_string(&plain)
                .unwrap()
                .contains("experimental")
        );
        let team = load_team("crosscheck", None, None).unwrap();
        assert!(team.file.experimental);
        assert_eq!(
            team.experimental_notice().as_deref(),
            Some("Crosscheck is experimental: see docs/research/")
        );
    }
}
