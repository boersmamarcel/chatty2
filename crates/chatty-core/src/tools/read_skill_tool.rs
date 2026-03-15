use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
    /// Which directory the skill was loaded from ("workspace" or "global")
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
/// 1. `<workspace>/.claude/skills/<name>/SKILL.md`  — project-local
/// 2. `<data_dir>/chatty/skills/<name>/SKILL.md`    — global user skills
///
/// For skills stored via the `save_skill` tool (not filesystem files), use
/// `search_memory` instead.
#[derive(Clone)]
pub struct ReadSkillTool {
    global_skills_dir: PathBuf,
    workspace_skills_dir: Option<PathBuf>,
}

impl ReadSkillTool {
    /// Create a new `ReadSkillTool`.
    ///
    /// `workspace_skills_dir` should be the `.claude/skills` directory inside the
    /// current workspace root, or `None` when no workspace is configured.
    pub fn new(workspace_skills_dir: Option<PathBuf>) -> Self {
        let global_skills_dir = dirs::data_dir()
            .map(|d| d.join("chatty").join("skills"))
            .unwrap_or_else(|| PathBuf::from(".chatty_skills"));
        Self {
            global_skills_dir,
            workspace_skills_dir,
        }
    }
}

impl Tool for ReadSkillTool {
    const NAME: &'static str = "read_skill";
    type Error = ReadSkillError;
    type Args = ReadSkillArgs;
    type Output = ReadSkillOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_skill".to_string(),
            description: "Load the full instructions for a named skill. \
                          Skills are listed with a one-line description in the automatic context \
                          block — use this tool to get the complete step-by-step procedure. \
                          Searches the workspace .claude/skills/ directory first, then the \
                          global skills directory. For skills created with save_skill (not \
                          filesystem files), use search_memory instead."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Exact name of the skill to load \
                                        (matches the subdirectory name, e.g. \"build-and-check\")."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let file_names = ["SKILL.md", "skill.md"];

        // Check workspace directory first
        if let Some(ref ws_dir) = self.workspace_skills_dir
            && let Some(content) =
                try_read_skill_file(&ws_dir.join(&args.name), &file_names).await
        {
            return Ok(ReadSkillOutput {
                content,
                source: "workspace".to_string(),
            });
        }

        // Fall back to global directory
        if let Some(content) =
            try_read_skill_file(&self.global_skills_dir.join(&args.name), &file_names).await
        {
            return Ok(ReadSkillOutput {
                content,
                source: "global".to_string(),
            });
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
        let tool = ReadSkillTool::new(None);
        let result = tool
            .call(ReadSkillArgs {
                name: "nonexistent-skill-xyz".to_string(),
            })
            .await;
        assert!(matches!(result, Err(ReadSkillError::NotFound(_))));
    }

    #[tokio::test]
    async fn reads_skill_from_workspace_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let content =
            "---\nname: my-skill\ndescription: A test skill.\n---\n# Steps\n1. Do something.";
        tokio::fs::write(skill_dir.join("SKILL.md"), content)
            .await
            .unwrap();

        let tool = ReadSkillTool::new(Some(tmp.path().to_path_buf()));
        let output = tool
            .call(ReadSkillArgs {
                name: "my-skill".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(output.content, content);
        assert_eq!(output.source, "workspace");
    }
}
