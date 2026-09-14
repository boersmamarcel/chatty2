//! The team directory: one place for a roster, the leader's role, the
//! verification command, the skill the leader follows and the turn budget
//! (ADR-0011 C13 / AGE-407).
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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::settings::models::module_settings::VirtualAgentConfig;
use crate::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};

/// The presets compiled into the binary: `(id, team.json, SKILL.md)`.
///
/// `coder-reviewer` names no models on purpose: they come from the roster's
/// default, `--model`, or a `team.json` of your own that overrides it.
pub const PRESETS: &[(&str, &str, &str)] = &[(
    "coder-reviewer",
    include_str!("../../teams/coder-reviewer/team.json"),
    include_str!("../../teams/coder-reviewer/SKILL.md"),
)];

/// The relative directory a team of that id lives in under a workspace.
pub const WORKSPACE_TEAMS_DIR: &str = ".chatty/teams";

/// The leader's own role for the run: which model it runs, which tool
/// profile and what its standing instructions are. Each half is optional
/// and an explicit `--model` / `--tools` / `--preamble` flag beats it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamLeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
}

/// `team.json` as written.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamFile {
    #[serde(default)]
    pub leader: TeamLeader,
    /// The roster, exactly as `module_settings.virtual_agents` declares one;
    /// it replaces that list for the run.
    #[serde(default)]
    pub agents: Vec<VirtualAgentConfig>,
    /// The team's verification command (`module_settings.team.verification`,
    /// AGE-406) for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// The skill the leader is told to read and follow on its first turn.
    /// Served by `read_skill` from the `SKILL.md` beside this file when
    /// there is one, else from the usual skill directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The leader's turn budget (`execution_settings.max_agent_turns`) for
    /// the run; the persisted default of 10 kills a delegating flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_turns: Option<u32>,
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

/// A loaded team: the file, where it came from, and the skill beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    pub id: String,
    pub source: TeamSource,
    pub file: TeamFile,
    /// The `SKILL.md` beside `team.json`, when there is one.
    pub skill_content: Option<String>,
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

    /// The names the run's broker publishes — the roster's, or the one
    /// default worker when the file declares none — exactly as
    /// `ModuleSettingsModel::virtual_agent_names` answers it for a declared
    /// roster.
    pub fn agent_names(&self) -> Vec<String> {
        self.run_module_settings(&ModuleSettingsModel::default())
            .virtual_agent_names()
    }

    /// The leader's turn budget for the run, when the file names one.
    /// Execution settings are the run's own copy on every host that reads a
    /// team, so this mutates in place.
    pub fn apply_turn_budget(&self, execution_settings: &mut ExecutionSettingsModel) {
        if let Some(turns) = self.file.max_agent_turns {
            execution_settings.max_agent_turns = turns;
        }
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
/// presets, in that order.
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
    for dir in candidates.iter().flatten() {
        let json = dir.join("team.json");
        if !json.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&json)
            .with_context(|| format!("failed to read {}", json.display()))?;
        let file: TeamFile = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a team file", json.display()))?;
        let skill_content = std::fs::read_to_string(dir.join("SKILL.md"))
            .ok()
            .filter(|s| !s.trim().is_empty());
        return Ok(Team {
            id: id.to_string(),
            source: TeamSource::Dir(dir.clone()),
            file,
            skill_content,
        });
    }
    if let Some((_, json, skill)) = PRESETS.iter().find(|(name, _, _)| *name == id) {
        let file = serde_json::from_str(json).expect("a preset team.json parses");
        return Ok(Team {
            id: id.to_string(),
            source: TeamSource::Preset,
            file,
            skill_content: Some(skill.to_string()),
        });
    }
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
            .map(|(name, _, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factories::agent_factory::tool_profile;

    fn write_team(dir: &Path, id: &str, json: &str, skill: Option<&str>) -> PathBuf {
        let team_dir = dir.join(id);
        std::fs::create_dir_all(&team_dir).unwrap();
        std::fs::write(team_dir.join("team.json"), json).unwrap();
        if let Some(skill) = skill {
            std::fs::write(team_dir.join("SKILL.md"), skill).unwrap();
        }
        team_dir
    }

    /// Do item 3: the shipped preset is the coder-reviewer team — a
    /// `coordinator` leader, `local-coder` on the `coder` profile,
    /// `local-reviewer` on the `reviewer` profile with the "verify, do not
    /// trust" preamble, the skill beside it, a 50-turn budget, and no
    /// models.
    #[test]
    fn the_coder_reviewer_preset_parses_and_names_its_roles() {
        let team = load_team("coder-reviewer", None, None).expect("the preset loads");
        assert_eq!(team.source, TeamSource::Preset);
        assert_eq!(team.file.leader.profile.as_deref(), Some("coordinator"));
        assert!(team.file.leader.model.is_none(), "no model in the preset");
        assert!(team.file.leader.preamble.is_some());

        let names: Vec<_> = team.file.agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["local-coder", "local-reviewer"]);
        assert_eq!(team.file.agents[0].tools.as_deref(), Some("coder"));
        assert_eq!(team.file.agents[1].tools.as_deref(), Some("reviewer"));
        assert!(
            team.file.agents[1]
                .preamble
                .as_deref()
                .unwrap()
                .starts_with("Verify, do not trust, verdict first."),
            "{:?}",
            team.file.agents[1].preamble
        );
        assert!(team.file.agents.iter().all(|a| a.model.is_none()));
        for agent in &team.file.agents {
            assert!(
                tool_profile(agent.tools.as_deref().unwrap()).is_some(),
                "{} names a real profile",
                agent.name
            );
        }
        assert!(tool_profile(team.file.leader.profile.as_deref().unwrap()).is_some());

        assert_eq!(team.file.max_agent_turns, Some(50));
        assert!(team.file.verification.is_none());
        let skill = team.skill().expect("the skill ships beside the preset");
        assert_eq!(skill.name, "coder-reviewer");
        assert!(
            skill.content.contains("git_merge"),
            "the repaired merge step"
        );
        assert!(skill.content.contains("`range`"), "the repaired diff step");
        assert!(
            skill.content.contains("default branch"),
            "default branch detection"
        );
        assert_eq!(
            team.first_turn_instruction().as_deref(),
            Some("read_skill coder-reviewer and follow it.")
        );
    }

    /// Do item 1: a team file in the workspace beats the preset of the same
    /// id, and the data directory sits between the two.
    #[test]
    fn a_workspace_team_overrides_the_preset_and_the_data_dir() {
        let workspace = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let ws_teams = workspace.path().join(WORKSPACE_TEAMS_DIR);
        let data_teams = data.path().join("chatty").join("teams");

        write_team(
            &data_teams,
            "coder-reviewer",
            r#"{"agents":[{"name":"data-coder"}],"skill":"coder-reviewer"}"#,
            Some("# data skill"),
        );
        let team = load_team("coder-reviewer", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(
            team.source,
            TeamSource::Dir(data_teams.join("coder-reviewer"))
        );
        assert_eq!(team.file.agents[0].name, "data-coder");
        assert_eq!(team.skill().unwrap().content, "# data skill");

        let ws_dir = write_team(
            &ws_teams,
            "coder-reviewer",
            r#"{
              "leader": {"model": "big", "profile": "coordinator", "preamble": "lead"},
              "agents": [{"name": "ws-coder", "tools": "coder"}],
              "verification": "cargo test",
              "skill": "coder-reviewer",
              "max_agent_turns": 7
            }"#,
            None,
        );
        let team = load_team("coder-reviewer", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(team.source, TeamSource::Dir(ws_dir));
        assert_eq!(team.file.leader.model.as_deref(), Some("big"));
        assert_eq!(team.file.agents[0].name, "ws-coder");
        assert_eq!(team.file.verification.as_deref(), Some("cargo test"));
        assert_eq!(team.file.max_agent_turns, Some(7));
        assert!(
            team.skill().is_none(),
            "no SKILL.md beside it: read_skill falls back to the skill dirs"
        );
        assert_eq!(
            team.first_turn_instruction().as_deref(),
            Some(
                "read_skill coder-reviewer and follow it. \
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
            virtual_agents: vec![VirtualAgentConfig {
                name: "stale".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        on_disk.team.verification = Some("old".to_string());
        let mut execution_settings = ExecutionSettingsModel::default();
        let before = execution_settings.max_agent_turns;

        let mut team = load_team("coder-reviewer", None, None).unwrap();
        team.file.max_agent_turns = None;
        let run = team.run_module_settings(&on_disk);
        assert_eq!(run.virtual_agent_names(), ["local-coder", "local-reviewer"]);
        assert_eq!(team.agent_names(), ["local-coder", "local-reviewer"]);
        assert!(run.team.verification.is_none());
        assert_eq!(
            run.gateway_port, 9999,
            "everything else is the on-disk value"
        );
        assert_eq!(on_disk.virtual_agent_names(), ["stale"]);
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
            "coder-reviewer",
            "{ not json",
            None,
        );
        let err = load_team("coder-reviewer", Some(workspace.path()), None).unwrap_err();
        assert!(err.to_string().contains("not a team file"), "{err:#}");
    }

    #[test]
    fn an_unknown_team_lists_where_it_looked_and_the_presets() {
        let workspace = tempfile::tempdir().unwrap();
        let err = load_team("nope", Some(workspace.path()), None).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(".chatty/teams/nope"), "{text}");
        assert!(text.contains("coder-reviewer"), "{text}");
        assert!(load_team("../x", None, None).is_err());
        assert!(load_team("", None, None).is_err());
    }
}
