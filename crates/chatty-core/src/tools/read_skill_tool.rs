use crate::services::memory_service::MemoryHitSource;
use crate::services::skill_service::SkillService;
use crate::services::team::TeamSkill;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Error type for the read_skill tool
#[derive(Debug, Error)]
pub enum ReadSkillError {
    #[error("Skill not found: \"{0}\"")]
    NotFound(String),
    #[error("Failed to read skill: {0}")]
    IoError(String),
}

impl Serialize for ReadSkillError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// Arguments for the read_skill tool
#[derive(Deserialize, Serialize)]
pub struct ReadSkillArgs {
    /// Name of the skill to read (must match the subdirectory name)
    pub name: String,
}

/// Output of the read_skill tool
#[derive(Debug, Serialize)]
pub struct ReadSkillOutput {
    /// The full content of the skill's SKILL.md file
    pub content: String,
    /// Where the skill was loaded from: "team", "workspace" or "global"
    pub source: String,
}

/// Tool that loads the full instructions for a named filesystem skill on demand.
///
/// The automatic context injection only includes a one-line description per
/// skill to keep the context window small.  Use this tool whenever you need
/// the complete step-by-step instructions for a skill that was listed in the
/// `[Relevant skills available]` block.
///
/// ## Skill locations searched (in order)
/// 0. The team's own skill, when this run was started with `--team` and the
///    team directory carries a `SKILL.md` (AGE-407)
/// 1. `.agents/skills/<name>/SKILL.md`, then `.claude/skills/<name>/SKILL.md`,
///    from the workspace up to its git root — project skills
/// 2. `~/.agents/skills/<name>/SKILL.md`, then `~/.claude/skills/<name>/SKILL.md`
///    — global skills
///
/// These are the directories [`SkillService`] lists for the slash-command
/// picker, so any skill the picker offers can be read here.
///
/// For skills stored via the `save_skill` tool (not filesystem files), use
/// `search_memory` instead.
#[derive(Clone)]
pub struct ReadSkillTool {
    skill_dirs: Vec<(PathBuf, MemoryHitSource)>,
    team_skill: Option<TeamSkill>,
}

impl ReadSkillTool {
    /// Create a new `ReadSkillTool`.
    ///
    /// `workspace_dir` is the workspace root, or `None` when no workspace is
    /// configured (global skills only).
    pub fn new(workspace_dir: Option<&Path>) -> Self {
        Self::with_skill_dirs(SkillService::new(None).skill_dirs(workspace_dir))
    }

    fn with_skill_dirs(skill_dirs: Vec<(PathBuf, MemoryHitSource)>) -> Self {
        Self {
            skill_dirs,
            team_skill: None,
        }
    }

    /// Serve the team directory's skill ahead of any file lookup (AGE-407):
    /// a preset compiled into the binary has no file for `read_skill` to
    /// find, and a team file in the workspace should win over a same-named
    /// skill in the skills directories anyway.
    pub fn with_team_skill(mut self, skill: Option<TeamSkill>) -> Self {
        self.team_skill = skill;
        self
    }
}

impl Tool for ReadSkillTool {
    const NAME: &'static str = "read_skill";
    type Error = ReadSkillError;
    type Args = ReadSkillArgs;
    type Output = ReadSkillOutput;

    fn description(&self) -> String {
        "Load the full instructions for a named skill. \
                          Skills are listed with a one-line description in the automatic context \
                          block — use this tool to get the complete step-by-step procedure. \
                          Searches the project's .agents/skills/ and .claude/skills/ first, \
                          then ~/.agents/skills/ and ~/.claude/skills/. For skills created with save_skill (not \
                          filesystem files), use search_memory instead."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Exact name of the skill to load \
                                    (matches the subdirectory name, e.g. \"build-and-check\")."
                }
            },
            "required": ["name"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let file_names = ["SKILL.md", "skill.md"];

        if let Some(skill) = self.team_skill.as_ref().filter(|s| s.name == args.name) {
            return Ok(ReadSkillOutput {
                content: skill.content.clone(),
                source: "team".to_string(),
            });
        }

        // A skill name is one directory name; anything else would read
        // outside the skills directories.
        if args.name.is_empty() || args.name.starts_with('.') || args.name.contains(['/', '\\']) {
            return Err(ReadSkillError::NotFound(args.name));
        }

        for (dir, source) in &self.skill_dirs {
            if let Some(content) = try_read_skill_file(&dir.join(&args.name), &file_names).await {
                let source = match source {
                    MemoryHitSource::GlobalSkillFile => "global",
                    _ => "workspace",
                };
                return Ok(ReadSkillOutput {
                    content,
                    source: source.to_string(),
                });
            }
        }

        Err(ReadSkillError::NotFound(args.name))
    }
}

/// Try to read any of `file_names` from `skill_dir`, returning the first non-empty file found.
async fn try_read_skill_file(skill_dir: &std::path::Path, file_names: &[&str]) -> Option<String> {
    for &file_name in file_names {
        match tokio::fs::read_to_string(skill_dir.join(file_name)).await {
            Ok(content) if !content.trim().is_empty() => return Some(content),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_is_read_skill() {
        assert_eq!(ReadSkillTool::NAME, "read_skill");
    }

    #[tokio::test]
    async fn returns_not_found_for_missing_skill() {
        let tool = ReadSkillTool::with_skill_dirs(Vec::new());
        let result = tool
            .call(
                &mut ToolContext::new(),
                ReadSkillArgs {
                    name: "nonexistent-skill-xyz".to_string(),
                },
            )
            .await;
        assert!(matches!(result, Err(ReadSkillError::NotFound(_))));
    }

    #[tokio::test]
    async fn reads_skill_from_workspace_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".claude/skills/my-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let content =
            "---\nname: my-skill\ndescription: A test skill.\n---\n# Steps\n1. Do something.";
        tokio::fs::write(skill_dir.join("SKILL.md"), content)
            .await
            .unwrap();

        let tool = ReadSkillTool::new(Some(tmp.path()));
        let output = tool
            .call(
                &mut ToolContext::new(),
                ReadSkillArgs {
                    name: "my-skill".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(output.content, content);
        assert_eq!(output.source, "workspace");
    }

    /// AGE-407: `--team` tells the leader "read_skill <skill> and follow
    /// it", and the skill it reads is the one beside `team.json` — ahead of
    /// a same-named workspace skill, and there at all for a preset compiled
    /// into the binary. Any other name still goes to the directories.
    #[tokio::test]
    async fn serves_the_team_skill_ahead_of_the_skill_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(".agents/skills/coder-reviewer");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), "# workspace copy")
            .await
            .unwrap();

        let tool = ReadSkillTool::new(Some(tmp.path())).with_team_skill(Some(TeamSkill {
            name: "coder-reviewer".to_string(),
            content: "# the team's copy".to_string(),
        }));
        let output = tool
            .call(
                &mut ToolContext::new(),
                ReadSkillArgs {
                    name: "coder-reviewer".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.content, "# the team's copy");
        assert_eq!(output.source, "team");

        let other = tool
            .call(
                &mut ToolContext::new(),
                ReadSkillArgs {
                    name: "something-else".to_string(),
                },
            )
            .await;
        assert!(matches!(other, Err(ReadSkillError::NotFound(_))));
    }

    #[tokio::test]
    async fn refuses_names_that_leave_the_skills_dir() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("SKILL.md"), "# outside")
            .await
            .unwrap();
        let tool = ReadSkillTool::with_skill_dirs(vec![(
            tmp.path().join("skills"),
            MemoryHitSource::GlobalSkillFile,
        )]);
        for name in ["..", "../", "a/b"] {
            let result = tool
                .call(
                    &mut ToolContext::new(),
                    ReadSkillArgs {
                        name: name.to_string(),
                    },
                )
                .await;
            assert!(matches!(result, Err(ReadSkillError::NotFound(_))), "{name}");
        }
    }
}
