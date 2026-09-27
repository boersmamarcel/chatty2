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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::agent_spec::{AgentSpec, load_agent_spec_from};
use crate::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};

/// The presets compiled into the binary: `(id, team.json, SKILL.md)`.
///
/// `coder-reviewer`'s specs name no models on purpose: they come from the
/// roster's default, `--model`, or a spec of your own that shadows one.
pub const PRESETS: &[(&str, &str, &str)] = &[(
    "coder-reviewer",
    include_str!("../../teams/coder-reviewer/team.json"),
    include_str!("../../teams/coder-reviewer/SKILL.md"),
)];

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
        && let Some((_, json, skill)) = PRESETS.iter().find(|(name, _, _)| *name == id)
    {
        let file = TeamFile::parse(json).expect("a preset team.json parses");
        found = Some((TeamSource::Preset, file, Some(skill.to_string())));
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
                .map(|(name, _, _)| *name)
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
    Ok(Team {
        id: id.to_string(),
        source,
        file,
        skill_content,
        leader,
        agents,
    })
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

    /// The shipped preset is the coder-reviewer team — a `coordinator`
    /// leader, `local-coder` on the `coder` profile, `local-reviewer` on the
    /// `reviewer` profile with the "verify, do not trust" preamble, the
    /// skill beside it, a 50-turn budget, and no models.
    #[test]
    fn the_coder_reviewer_preset_parses_and_names_its_roles() {
        let team = load_team("coder-reviewer", None, None).expect("the preset loads");
        assert_eq!(team.source, TeamSource::Preset);
        assert_eq!(team.file.leader, "coder-reviewer-leader");
        assert_eq!(team.leader.tools.profile.as_deref(), Some("coordinator"));
        assert!(team.leader.agent.model.is_none(), "no model in the preset");
        assert!(team.leader.agent.preamble.is_some());

        assert_eq!(team.file.agents, ["local-coder", "local-reviewer"]);
        let names: Vec<_> = team.agents.iter().map(|a| a.agent.name.as_str()).collect();
        assert_eq!(names, ["local-coder", "local-reviewer"]);
        assert_eq!(team.agents[0].tools.profile.as_deref(), Some("coder"));
        assert_eq!(team.agents[1].tools.profile.as_deref(), Some("reviewer"));
        assert!(
            team.agents[1]
                .agent
                .preamble
                .as_deref()
                .unwrap()
                .starts_with("Verify, do not trust, verdict first."),
            "{:?}",
            team.agents[1].agent.preamble
        );
        assert!(team.agents.iter().all(|a| a.agent.model.is_none()));

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
            "coder-reviewer",
            r#"{"leader":"coder-reviewer-leader","agents":["data-coder"],"skill":"coder-reviewer"}"#,
            Some("# data skill"),
        );
        let team = load_team("coder-reviewer", Some(workspace.path()), Some(data.path())).unwrap();
        assert_eq!(
            team.source,
            TeamSource::Dir(data_teams.join("coder-reviewer"))
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
            "coder-reviewer-leader",
            "model = \"big\"\npreamble = \"lead\"\n[tools]\nprofile = \"coordinator\"\n",
        );
        let ws_dir = write_team(
            &ws_teams,
            "coder-reviewer",
            r#"{
              "leader": "coder-reviewer-leader",
              "agents": ["ws-coder"],
              "verification": "cargo test",
              "skill": "coder-reviewer",
              "max_agent_turns": 7
            }"#,
            None,
        );
        let team = load_team("coder-reviewer", Some(workspace.path()), Some(data.path())).unwrap();
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
            virtual_agents: vec!["stale".to_string()],
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
                r#"{"leader":"coder-reviewer-leader","agents":[{"name":"local-coder","tools":"coder"}]}"#,
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

    #[test]
    fn a_team_naming_a_missing_spec_says_which() {
        let workspace = tempfile::tempdir().unwrap();
        write_team(
            &workspace.path().join(WORKSPACE_TEAMS_DIR),
            "t",
            r#"{"leader":"coder-reviewer-leader","agents":["nobody"]}"#,
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
        assert!(text.contains("coder-reviewer"), "{text}");
        assert!(load_team("../x", None, None).is_err());
        assert!(load_team("", None, None).is_err());
    }
}
