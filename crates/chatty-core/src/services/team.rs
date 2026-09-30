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
/// spec of your own that shadows one; the exception is
/// `architecture-review` (AGE-808), whose specs pin hosted models and which
/// [`check_model_providers`] refuses to start without that provider. A preset without a `SKILL.md` has its
/// playbook in the leader's preamble, so `/agent <leader>` runs it the same
/// as `--team <id>`.
pub const PRESETS: &[TeamPreset] = &[
    TeamPreset {
        id: "data-analysis",
        team_json: include_str!("../../teams/data-analysis/team.json"),
        skill: None,
        schemas: &[],
    },
    TeamPreset {
        id: "research-brief",
        team_json: include_str!("../../teams/research-brief/team.json"),
        skill: None,
        schemas: &[],
    },
    TeamPreset {
        id: "fix-and-verify",
        team_json: include_str!("../../teams/fix-and-verify/team.json"),
        skill: None,
        schemas: &[],
    },
    TeamPreset {
        id: "analyst-panel",
        team_json: include_str!("../../teams/analyst-panel/team.json"),
        skill: Some(include_str!("../../teams/analyst-panel/SKILL.md")),
        schemas: &[
            (
                "schemas/analyst.json",
                include_str!("../../teams/analyst-panel/schemas/analyst.json"),
            ),
            (
                "schemas/adjudicator.json",
                include_str!("../../teams/analyst-panel/schemas/adjudicator.json"),
            ),
        ],
    },
    TeamPreset {
        id: "architecture-review",
        team_json: include_str!("../../teams/architecture-review/team.json"),
        skill: Some(include_str!("../../teams/architecture-review/SKILL.md")),
        schemas: &[
            (
                "schemas/proposer.json",
                include_str!("../../teams/architecture-review/schemas/proposer.json"),
            ),
            (
                "schemas/review.json",
                include_str!("../../teams/architecture-review/schemas/review.json"),
            ),
            (
                "schemas/verify.json",
                include_str!("../../teams/architecture-review/schemas/verify.json"),
            ),
        ],
    },
];

/// The relative directory a team of that id lives in under a workspace.
pub const WORKSPACE_TEAMS_DIR: &str = ".chatty/teams";

/// `team.json` as written: a thin file of agent spec names (AGE-614).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamFile {
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
                    return file.verification;
                }
            }
        }
    }
    for preset in PRESETS {
        let file = TeamFile::parse(preset.team_json).expect("a preset team.json parses");
        if file.leader == name || file.agents.iter().any(|agent| agent == name) {
            return file.verification;
        }
    }
    None
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
            "no team '{id}': looked in {}, and the presets are {}",
            candidates
                .iter()
                .flatten()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            PRESETS
                .iter()
                .map(|preset| preset.id)
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    let spec = |name: &str| {
        load_agent_spec_from(name, workspace, data_dir)
            .map(|loaded| loaded.spec)
            .with_context(|| format!("team '{id}' names agent spec '{name}'"))
    };
    let leader = spec(&file.leader)?;
    let agents = file
        .agents
        .iter()
        .map(|name| spec(name))
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
                schema,
            },
        );
    }
    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::WORKSPACE_AGENTS_DIR;
    use crate::services::architecture_doc;

    fn write_team(dir: &Path, id: &str, json: &str, skill: Option<&str>) -> PathBuf {
        let team_dir = dir.join(id);
        std::fs::create_dir_all(&team_dir).unwrap();
        std::fs::write(team_dir.join("team.json"), json).unwrap();
        if let Some(skill) = skill {
            std::fs::write(team_dir.join("SKILL.md"), skill).unwrap();
        }
        team_dir
    }

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
        let analysis = load_team("data-analysis", None, None).unwrap();
        assert_eq!(analysis.file.leader, "data-lead");
        assert_eq!(analysis.file.agents, ["data-analyst", "reviewer"]);
        assert!(analysis.skill().is_none(), "the playbook is the preamble");
        let brief = load_team("research-brief", None, None).unwrap();
        assert_eq!(brief.file.leader, "editor");
        assert_eq!(brief.file.agents, ["researcher", "writer", "reviewer"]);
    }

    /// AGE-757: `fix-and-verify` is a coding team whose tests Chatty runs:
    /// the verification command is the fixture's own test suite, the coder
    /// has a shell (so its tree is checked) and the reviewer has none (it
    /// judges the diff and Chatty's evidence, not a run of its own).
    #[test]
    fn fix_and_verify_runs_the_projects_tests_on_the_coders_tree() {
        let team = load_team("fix-and-verify", None, None).unwrap();
        assert_eq!(team.file.leader, "fix-lead");
        assert_eq!(team.file.agents, ["fix-coder", "code-reviewer"]);
        assert_eq!(
            team.file.verification.as_deref(),
            Some("python3 -m unittest discover -s tests -t . -v")
        );
        let tools = |name: &str| {
            let spec = team.agents.iter().find(|s| s.agent.name == name).unwrap();
            (spec.tools.profile.clone(), spec.tools.disable.clone())
        };
        assert_eq!(tools("fix-coder"), (Some("coder".into()), vec![]));
        assert_eq!(
            tools("code-reviewer"),
            (Some("reviewer".into()), vec!["shell".to_string()])
        );
    }

    /// AGE-757: the fixture has one known bug: an order of exactly the
    /// free-shipping threshold is charged shipping (`>` where the docstring
    /// says "or more"). One test fails on it and the other three pass; the
    /// fix is `>=` in `invoice.py`, with the tests untouched.
    #[test]
    fn the_fix_and_verify_fixture_has_one_known_bug() {
        let code = include_str!("../../teams/fix-and-verify/fixture/invoice.py");
        let tests = include_str!("../../teams/fix-and-verify/fixture/tests/test_invoice.py");
        assert!(code.contains("if subtotal > FREE_SHIPPING_FROM:"));
        assert!(code.contains("FREE_SHIPPING_FROM or more ships free"));
        assert_eq!(tests.matches("    def test_").count(), 4);
        assert!(tests.contains("def test_order_of_exactly_the_threshold_ships_free"));
    }

    /// AGE-752: `data-analysis`'s sample orders have a known answer, so a
    /// run on them has a right one: August revenue is 17.6% below July's,
    /// and two planted causes carry almost all of the fall — EU `Pro` sales
    /// stop from 11 August (about 70%) and the online `SUMMER30` code takes
    /// 30% off `Starter` from 1 August without lifting volume (about 27%).
    /// The fixture is frozen with the preset.
    #[test]
    fn the_data_analysis_orders_have_a_known_answer() {
        let csv = include_str!("../../teams/data-analysis/fixture/orders.csv");
        let mut lines = csv.lines();
        assert_eq!(
            lines.next(),
            Some(
                "order_id,date,region,channel,product,units,unit_price,\
                 discount_pct,promo_code,customer_type,revenue"
            )
        );
        let mut total = [0.0f64; 2];
        let mut eu_pro = [0.0f64; 2];
        let mut online_starter = [0.0f64; 2];
        let mut rows = 0;
        for line in lines {
            let f: Vec<&str> = line.split(',').collect();
            let month = usize::from(f[1].starts_with("2026-08"));
            let revenue: f64 = f[10].parse().expect("a revenue");
            total[month] += revenue;
            if f[2] == "EU" && f[4] == "Pro" {
                eu_pro[month] += revenue;
            }
            if f[3] == "online" && f[4] == "Starter" {
                online_starter[month] += revenue;
            }
            rows += 1;
        }
        assert_eq!(rows, 793);
        let fall = total[1] - total[0];
        assert!(((fall / total[0]) * 100.0 + 17.6).abs() < 0.05, "{fall}");
        let share = |seg: [f64; 2]| (seg[1] - seg[0]) / fall;
        assert!((0.69..0.72).contains(&share(eu_pro)), "{}", share(eu_pro));
        assert!(
            (0.26..0.29).contains(&share(online_starter)),
            "{}",
            share(online_starter)
        );
    }

    /// The analyst-panel preset (AGE-754): a coordinator leader allowed to
    /// delegate to exactly its roster, three identical analysts with turn
    /// and time budgets inside the leader's deadline, a read-only
    /// adjudicator, a writer, and both handoff schemas compiled in.
    #[test]
    fn the_analyst_panel_preset_loads_with_its_handoffs_and_budgets() {
        let team = load_team("analyst-panel", None, None).expect("the preset loads");
        assert_eq!(team.source, TeamSource::Preset);
        assert_eq!(team.file.leader, "panel-lead");
        assert_eq!(team.leader.tools.profile.as_deref(), Some("coordinator"));
        assert_eq!(team.leader.swarm.delegates_to, team.file.agents);
        assert_eq!(team.leader.budget.max_duration.as_deref(), Some("45m"));
        assert!(team.leader.agent.model.is_none(), "no model in the preset");

        let analysts: Vec<_> = team
            .agents
            .iter()
            .filter(|a| a.agent.name.starts_with("panel-analyst-"))
            .collect();
        assert_eq!(analysts.len(), 3);
        for analyst in &analysts {
            assert_eq!(analyst.tools.profile.as_deref(), Some("coder"));
            assert_eq!(analyst.budget.max_agent_turns, Some(30));
            assert_eq!(analyst.budget.max_duration.as_deref(), Some("12m"));
            assert_eq!(
                analyst.swarm.callers.as_deref(),
                Some(&["panel-lead".to_string()][..])
            );
            assert!(analyst.agent.model.is_none());
            // Identical but for the name: a homogeneous panel.
            assert_eq!(analyst.agent.preamble, analysts[0].agent.preamble);
        }
        let adjudicator = team
            .agents
            .iter()
            .find(|a| a.agent.name == "panel-adjudicator")
            .unwrap();
        assert_eq!(adjudicator.tools.profile.as_deref(), Some("coordinator"));
        assert!(
            adjudicator.swarm.delegates_to.is_empty(),
            "the adjudicator delegates to nobody"
        );

        assert_eq!(
            team.handoffs.keys().collect::<Vec<_>>(),
            [
                "panel-adjudicator",
                "panel-analyst-1",
                "panel-analyst-2",
                "panel-analyst-3"
            ]
        );
        assert!(team.handoff_ledger().is_some());
        assert_eq!(
            team.first_turn_instruction().as_deref(),
            Some("read_skill analyst-panel and follow it.")
        );
        let skill = team.skill().unwrap();
        assert!(skill.content.contains("include_trace: true"));
        assert!(skill.content.contains("## When a step fails"));
    }

    /// The analyst handoff is what turns a reply cut off mid-sentence into
    /// an invalid handoff (and its one re-prompt) instead of a lost answer;
    /// the adjudicator's names a candidate by number.
    #[test]
    fn the_analyst_panel_handoffs_reject_a_truncated_reply() {
        let team = load_team("analyst-panel", None, None).unwrap();
        let analyst = &team.handoffs["panel-analyst-2"];
        assert!(matches!(
            handoff::check(analyst, "The"),
            handoff::HandoffOutcome::Invalid { .. }
        ));
        assert!(matches!(
            handoff::check(
                analyst,
                "```json\n{\"answer\": \"\", \"method\": \"sum\"}\n```"
            ),
            handoff::HandoffOutcome::Invalid { .. }
        ));
        assert_eq!(
            handoff::check(
                analyst,
                "Done.\n```json\n{\"answer\": \"42.5\", \"method\": \"sum of amount\", \"assumptions\": \"\"}\n```"
            ),
            handoff::HandoffOutcome::Valid(serde_json::json!({
                "answer": "42.5", "method": "sum of amount", "assumptions": ""
            }))
        );

        let adjudicator = &team.handoffs["panel-adjudicator"];
        assert!(matches!(
            handoff::check(
                adjudicator,
                "```json\n{\"choice\": 4, \"answer\": \"x\", \"reason\": \"y\"}\n```"
            ),
            handoff::HandoffOutcome::Invalid { .. }
        ));
        assert!(matches!(
            handoff::check(
                adjudicator,
                "```json\n{\"choice\": 2, \"answer\": \"x\", \"reason\": \"y\"}\n```"
            ),
            handoff::HandoffOutcome::Valid(_)
        ));
        // Only what the leader acts on is required: a decision without its
        // reason still delivers an answer (a dev-10 smoke lost one that way).
        assert!(matches!(
            handoff::check(
                adjudicator,
                "```json\n{\"choice\": 1, \"answer\": \"x\"}\n```"
            ),
            handoff::HandoffOutcome::Valid(_)
        ));
        assert!(matches!(
            handoff::check(analyst, "```json\n{\"answer\": \"NL\"}\n```"),
            handoff::HandoffOutcome::Valid(_)
        ));
    }

    const STRONGEST_CLAUDE: &str = "anthropic/claude-opus-5";
    const CHEAPER_CLAUDE: &str = "anthropic/claude-sonnet-5";
    const ARCH_REVIEWERS: [&str; 3] = [
        "arch-maint-reviewer",
        "arch-sec-reviewer",
        "arch-devils-advocate",
    ];

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

    fn arch_specs(team: &Team) -> Vec<&AgentSpec> {
        std::iter::once(&team.leader).chain(&team.agents).collect()
    }

    /// The architecture-review preset (AGE-808): a coordinator leader that
    /// delegates to exactly its roster, a proposer that owns the document
    /// (the only writer), three blank read-only reviewers, a read-only
    /// verifier, all callable by the leader only; typed handoffs for every
    /// worker; and the first pinned models of any preset: the strongest
    /// Claude model for every role but the verifier, which checks one diff
    /// on a cheaper one.
    #[test]
    fn the_architecture_review_preset_loads_with_its_roster_handoffs_and_pinned_models() {
        let team = load_team("architecture-review", None, None).expect("the preset loads");
        assert_eq!(team.source, TeamSource::Preset);
        assert_eq!(team.file.leader, "arch-lead");
        assert_eq!(
            team.file.agents,
            [
                "arch-proposer",
                "arch-maint-reviewer",
                "arch-sec-reviewer",
                "arch-devils-advocate",
                "arch-verifier"
            ]
        );
        assert_eq!(team.file.max_agent_turns, Some(160));
        assert_eq!(team.leader.tools.profile.as_deref(), Some("coordinator"));
        assert_eq!(team.leader.swarm.delegates_to, team.file.agents);
        assert!(team.leader.budget.max_duration.is_some());
        let spec = |name: &str| team.agents.iter().find(|a| a.agent.name == name).unwrap();
        for worker in &team.agents {
            assert_eq!(
                worker.swarm.callers.as_deref(),
                Some(&["arch-lead".to_string()][..]),
                "{}",
                worker.agent.name
            );
            assert!(
                worker.budget.max_agent_turns.is_some() && worker.budget.max_duration.is_some()
            );
        }

        // The proposer writes; everyone else only reads.
        let proposer = spec("arch-proposer");
        assert_eq!(proposer.tools.profile.as_deref(), Some("coder"));
        assert_eq!(proposer.tools.disable, ["execute_code", "git"]);
        for reviewer in ARCH_REVIEWERS {
            assert_eq!(
                spec(reviewer).tools.profile.as_deref(),
                Some("reviewer"),
                "{reviewer}"
            );
            assert!(spec(reviewer).tools.disable.is_empty(), "{reviewer}");
        }
        let verifier = spec("arch-verifier");
        assert_eq!(verifier.tools.profile.as_deref(), Some("reviewer"));
        assert_eq!(verifier.tools.disable, ["shell"]);

        // The proposer's templates are the format `architecture_doc` checks.
        let preamble = proposer.agent.preamble.as_deref().unwrap();
        for key in architecture_doc::ADR_KEYS
            .iter()
            .chain(architecture_doc::DESIGN_KEYS)
        {
            assert!(
                preamble.contains(&format!("\n{key}: ")),
                "the templates carry `{key}:`"
            );
        }
        for (level, heading) in architecture_doc::ADR_HEADINGS
            .iter()
            .chain(architecture_doc::DESIGN_HEADINGS)
        {
            let line = format!("\n{} {heading}", "#".repeat(*level));
            assert!(preamble.contains(&line), "the templates carry {line:?}");
        }
        assert!(preamble.contains("status: proposed"));
        assert!(preamble.contains("docs/adr/ADR-NNNN-slug.md"));

        // Pinned models: they resolve among the models the OpenRouter sync
        // makes, which is what `spec.validate` checks.
        let models = synced_claude_models();
        for member in arch_specs(&team) {
            let name = &member.agent.name;
            let expected = if name == "arch-verifier" {
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

        assert_eq!(
            team.handoffs.keys().collect::<Vec<_>>(),
            [
                "arch-devils-advocate",
                "arch-maint-reviewer",
                "arch-proposer",
                "arch-sec-reviewer",
                "arch-verifier"
            ]
        );
        for reviewer in ARCH_REVIEWERS {
            assert_eq!(
                team.handoffs[reviewer].schema, team.handoffs["arch-sec-reviewer"].schema,
                "the three reviewers share one schema"
            );
        }
        assert!(team.handoff_ledger().is_some());
        assert_eq!(
            team.first_turn_instruction().as_deref(),
            Some("read_skill architecture-review and follow it.")
        );
        let skill = team.skill().unwrap();
        assert!(skill.content.contains("## When a step fails"));
        assert!(
            skill
                .content
                .contains("delegate to the same reviewer once more with a new persona")
        );
        assert!(
            skill
                .content
                .contains("## Personas (one per round, in order)")
        );
        assert!(skill.content.contains("**Devil's advocate:**"));
        assert!(skill.content.contains("record the gap"));
        assert!(
            skill.content.contains("`ask_user`"),
            "product questions go to the human"
        );
    }

    /// The handoffs are small and flat on purpose (E8, AGE-754: invalid
    /// handoffs were the panel's biggest loss): each rejects a malformed
    /// reply and accepts a valid one, and requires only what the leader
    /// acts on.
    #[test]
    fn the_architecture_review_handoffs_reject_a_malformed_reply_and_accept_a_valid_one() {
        let team = load_team("architecture-review", None, None).unwrap();
        let invalid = |role: &str, reply: &str| {
            assert!(
                matches!(
                    handoff::check(&team.handoffs[role], reply),
                    handoff::HandoffOutcome::Invalid { .. }
                ),
                "{role} must reject {reply:?}"
            );
        };
        let valid = |role: &str, reply: &str| match handoff::check(&team.handoffs[role], reply) {
            handoff::HandoffOutcome::Valid(value) => value,
            other => panic!("{role} must accept {reply:?}: {other:?}"),
        };

        for reviewer in ARCH_REVIEWERS {
            invalid(reviewer, "The draft looks good.");
            invalid(reviewer, "```json\n{\"findings\": \"1. must-fix: x\"}\n```");
            invalid(
                reviewer,
                "```json\n{\"must_fix\": -1, \"findings\": \"x\"}\n```",
            );
            invalid(
                reviewer,
                "```json\n{\"must_fix\": 0, \"findings\": \"\"}\n```",
            );
            invalid(
                reviewer,
                "```json\n{\"must_fix\": 0, \"findings\": \"ok\", \"verdict\": \"LGTM\"}\n```",
            );
            invalid(
                reviewer,
                "```json\n{\"must_fix\": 1, \"findings\": [{\"severity\": \"must-fix\"}]}\n```",
            );
            // Only the count the leader loops on and the text the proposer
            // reads are required.
            valid(
                reviewer,
                "```json\n{\"must_fix\": 0, \"findings\": \"No findings: the decision holds.\"}\n```",
            );
            let review = valid(
                reviewer,
                "Review done.\n```json\n{\"verdict\": \"REWORK\", \"must_fix\": 1, \"should_fix\": 1, \"findings\": \"1. must-fix: Decision says the flag isolates the agent; nothing enforces it. Evidence: crates/chatty-core/src/services/a2a_client.rs:40. Fix: say what enforces it.\\n2. should-fix: no owner for the allowlist.\"}\n```",
            );
            assert_eq!(review["must_fix"], 1);
        }

        let proposer = "arch-proposer";
        invalid(proposer, "I revised the document.");
        invalid(
            proposer,
            "```json\n{\"accepted\": 1, \"rejected\": 0, \"human_questions\": [], \"summary\": \"x\"}\n```",
        );
        invalid(
            proposer,
            "```json\n{\"accepted\": 1, \"rejected\": 0, \"rejected_must_fix\": 0, \"human_questions\": \"none\", \"summary\": \"x\"}\n```",
        );
        invalid(
            proposer,
            "```json\n{\"accepted\": 0, \"rejected\": 0, \"rejected_must_fix\": 0, \"human_questions\": [], \"summary\": \"\"}\n```",
        );
        let report = valid(
            proposer,
            "Written.\n```json\n{\"accepted\": 2, \"partial\": 1, \"rejected\": 1, \"rejected_must_fix\": 1, \"words\": 900, \"human_questions\": [\"Default on or off? I recommend off.\"], \"summary\": \"Named the owner; rejected F3: ssrf_guard.rs:30 already blocks it.\"}\n```",
        );
        assert_eq!(report["rejected_must_fix"], 1);
        valid(
            proposer,
            "```json\n{\"accepted\": 0, \"rejected\": 0, \"rejected_must_fix\": 0, \"human_questions\": [], \"summary\": \"First draft.\"}\n```",
        );

        let verifier = "arch-verifier";
        invalid(verifier, "Looks like wording only.");
        invalid(verifier, "```json\n{\"verdict\": \"PASS\"}\n```");
        invalid(
            verifier,
            "```json\n{\"verdict\": \"ok\", \"blockers\": []}\n```",
        );
        valid(
            verifier,
            "```json\n{\"verdict\": \"PASS\", \"blockers\": []}\n```",
        );
        valid(
            verifier,
            "```json\n{\"verdict\": \"FAIL\", \"blockers\": [\"Kill criteria: two releases became three; restore two.\"]}\n```",
        );
    }

    /// The loop rule is the leader's: a coordinator model follows the skill,
    /// and a scripted model would only replay whatever decisions a test
    /// scripts, so what pins the rule is the skill's own text. A round with
    /// no must-fix ends the review; a must-fix the proposer rejected with
    /// evidence does not count; the two safety valves and the 10-round cap
    /// end it as not converged; the review file records every round.
    #[test]
    fn the_architecture_review_skill_states_the_loop_rule() {
        let team = load_team("architecture-review", None, None).unwrap();
        let skill = team.skill().unwrap().content;
        let step = |n: usize| {
            skill
                .split(&format!("\n{n}. **"))
                .nth(1)
                .and_then(|rest| rest.split(&format!("\n{}. **", n + 1)).next())
                .unwrap_or_else(|| panic!("the skill has a step {n}"))
        };
        let count = step(3);
        assert!(
            count.contains("sum of the three reviewers' `must_fix`"),
            "{count}"
        );
        assert!(
            count.contains("`M(N)` is 0 and all three reviewers returned a review")
                && count.contains("**converged**"),
            "{count}"
        );
        let revise = step(4);
        assert!(
            revise.contains("Subtract its `rejected_must_fix`"),
            "{revise}"
        );
        assert!(revise.contains("**converged**"), "{revise}");
        let rule = step(5);
        let clauses: Vec<&str> = rule
            .lines()
            .filter(|l| l.trim_start().starts_with("- "))
            .collect();
        assert_eq!(clauses.len(), 4, "{rule}");
        assert!(clauses[0].contains("`M(N-1) >= M(N-2)` and `M(N) >= M(N-1)`"));
        assert!(clauses[1].contains("accepted in an earlier round"));
        assert!(clauses[2].contains("`N` is 10"));
        for valve in &clauses[..3] {
            assert!(valve.contains("**not converged**"), "{valve}");
        }
        assert!(clauses[3].contains("step 2 with N + 1"));
        assert!(skill.contains("the round does not converge, even at 0 must-fix"));
        let record = step(8);
        assert!(record.contains("Round <N>: <M(N)> must-fix"), "{record}");
        assert!(
            record.contains("## Open questions for the human"),
            "{record}"
        );
        assert!(skill.contains("`.review.md`"));
        assert!(skill.contains("Never call a not-converged review done"));
        assert!(skill.contains("never saved up for the end"));
    }

    /// AGE-808: without the provider a pinned model needs, the team fails
    /// before anything runs, naming the provider and the agents; with it, or
    /// with every spec shadowed by one without a pin, it passes.
    #[test]
    fn a_pinned_model_without_its_provider_names_the_provider() {
        let team = load_team("architecture-review", None, None).unwrap();
        let text = |models: &[ModelConfig], providers: &[ProviderConfig]| {
            format!(
                "{:#}",
                check_model_providers(arch_specs(&team), models, providers).unwrap_err()
            )
        };

        // No OpenRouter at all, so the sync never added the models.
        let error = text(&[], &[]);
        assert!(error.contains("OpenRouter is not configured"), "{error}");
        assert!(
            error.contains("arch-lead (model = \"anthropic/claude-opus-5\")"),
            "{error}"
        );
        assert!(
            error.contains("arch-verifier (model = \"anthropic/claude-sonnet-5\")"),
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
            arch_specs(&team),
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
            error.contains("arch-proposer (model = \"anthropic/claude-opus-5\")"),
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

        // Every spec shadowed without a pin: the roster's default runs it.
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(WORKSPACE_AGENTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        for member in arch_specs(&team) {
            let mut shadow = member.clone();
            shadow.agent.model = None;
            std::fs::write(
                dir.join(format!("{}.toml", shadow.agent.name)),
                shadow.to_toml().unwrap(),
            )
            .unwrap();
        }
        let shadowed = load_team("architecture-review", Some(workspace.path()), None).unwrap();
        check_model_providers(arch_specs(&shadowed), &[], &[])
            .expect("a shadowed team needs no hosted provider");
    }

    /// AGE-808: `--model` on a team run is the one-flag way onto a single
    /// model: every member runs it, the preset's pins included; without it
    /// the pins stand.
    #[test]
    fn model_for_the_run_replaces_every_members_pin() {
        let team = load_team("architecture-review", None, None).unwrap();
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
        let data = load_team("data-analysis", None, None).unwrap();
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
            "data-analysis",
            r#"{"leader":"data-lead","agents":["data-coder"],"skill":"data-analysis"}"#,
            Some("# data skill"),
        );
        let team = load_team("data-analysis", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(
            team.source,
            TeamSource::Dir(data_teams.join("data-analysis"))
        );
        assert_eq!(team.agents[0].agent.name, "data-coder");
        assert_eq!(team.skill().unwrap().content, "# data skill");

        write_spec(
            workspace.path(),
            "ws-coder",
            "[tools]\nprofile = \"coder\"\n",
        );
        write_spec(
            workspace.path(),
            "data-lead",
            "model = \"big\"\npreamble = \"lead\"\n[tools]\nprofile = \"coordinator\"\n",
        );
        let ws_dir = write_team(
            &ws_teams,
            "data-analysis",
            r#"{
              "leader": "data-lead",
              "agents": ["ws-coder"],
              "verification": "cargo test",
              "skill": "data-analysis",
              "max_agent_turns": 7
            }"#,
            None,
        );
        let team = load_team("data-analysis", Some(workspace.path()), Some(data.path())).unwrap();
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
                "read_skill data-analysis and follow it. \
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

        let mut team = load_team("data-analysis", None, None).unwrap();
        team.file.max_agent_turns = None;
        let run = team.run_module_settings(&on_disk);
        assert_eq!(run.virtual_agents, ["data-analyst", "reviewer"]);
        assert_eq!(team.agent_names(), ["data-analyst", "reviewer"]);
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
            "data-analysis",
            "{ not json",
            None,
        );
        let err = load_team("data-analysis", Some(workspace.path()), None).unwrap_err();
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
                r#"{"leader":"data-lead","agents":[{"name":"local-coder","tools":"coder"}]}"#,
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
                r#"{{"leader":"data-lead","agents":["data-analyst","reviewer"],"handoffs":{handoffs}}}"#
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
            r#"{"type":"object","x-must-be-read":{"data-analyst":["files_changed"]}}"#,
        )
        .unwrap();

        for (handoffs, says) in [
            (
                r#"{"data-analyst":"schemas/missing.json"}"#,
                "failed to read",
            ),
            (r#"{"data-analyst":"schemas/not-json.json"}"#, "is not JSON"),
            (
                r#"{"data-analyst":"schemas/bad.json"}"#,
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
            &team_json(
                r#"{"data-analyst":"schemas/change.json","reviewer":"schemas/review.json"}"#,
            ),
            None,
        );
        let team = load_team("t", Some(workspace.path()), None).unwrap();
        assert_eq!(team.handoffs["data-analyst"].role, "data-analyst");
        assert_eq!(
            team.handoffs["data-analyst"].schema["required"][0],
            "files_changed"
        );
        assert!(team.handoff_ledger().is_some());
        let preset = load_team("data-analysis", None, None).unwrap();
        assert!(preset.handoffs.is_empty() && preset.handoff_ledger().is_none());
    }

    #[test]
    fn a_team_naming_a_missing_spec_says_which() {
        let workspace = tempfile::tempdir().unwrap();
        write_team(
            &workspace.path().join(WORKSPACE_TEAMS_DIR),
            "t",
            r#"{"leader":"data-lead","agents":["nobody"]}"#,
            None,
        );
        let err = format!(
            "{:#}",
            load_team("t", Some(workspace.path()), None).unwrap_err()
        );
        assert!(err.contains("'nobody'"), "{err}");
    }

    #[test]
    fn an_unknown_team_lists_where_it_looked_and_the_presets() {
        let workspace = tempfile::tempdir().unwrap();
        let err = load_team("nope", Some(workspace.path()), None).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(".chatty/teams/nope"), "{text}");
        assert!(text.contains("data-analysis"), "{text}");
        assert!(load_team("../x", None, None).is_err());
        assert!(load_team("", None, None).is_err());
    }
}
