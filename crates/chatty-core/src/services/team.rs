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
use crate::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};

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
/// models on purpose: they come from the roster's default, `--model`, or a
/// spec of your own that shadows one. A preset without a `SKILL.md` has its
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

    /// AGE-760: a preset team's specs are on no default roster; selecting
    /// the team (`--team <id>`) is what puts them on the run's roster, and
    /// its leader, the root, reaches every one of them.
    #[test]
    fn selecting_a_team_adds_its_presets() {
        let default_roster = crate::agent_spec::roster_names_from(&[], None, None);
        for preset in PRESETS {
            let id = preset.id;
            let team = load_team(id, None, None).expect("the preset loads");
            for name in team.file.agents.iter().chain([&team.file.leader]) {
                assert!(
                    !default_roster.contains(name),
                    "{id}: {name} is not on the default roster"
                );
            }
            assert_eq!(team.agent_names(), team.file.agents, "{id}");
            let settings = team.run_module_settings(&ModuleSettingsModel::default());
            let roster = crate::agent_spec::load_roster_from(&settings.virtual_agents, None, None)
                .expect("the team's roster loads");
            assert_eq!(roster, team.agents, "{id}: the run serves the team");
        }
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
            gateway_port: 9999,
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
            run.gateway_port, 9999,
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
