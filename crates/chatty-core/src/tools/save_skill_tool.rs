use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::services::embedding_service::EmbeddingService;
use crate::services::memory_service::MemoryService;
use crate::tools::ToolError;

/// Title prefix used to mark skill entries in memory.
/// The injection code uses this prefix to partition facts from procedures.
pub const SKILL_TITLE_PREFIX: &str = "[SKILL] ";

/// Arguments for the save_skill tool
#[derive(Deserialize, Serialize)]
pub struct SaveSkillArgs {
    /// Short memorable name for this skill (e.g., "deploy to production")
    pub name: String,
    /// Ordered list of steps to perform this task
    pub steps: Vec<String>,
    /// One-sentence description of when to use this skill
    pub description: String,
}

/// Tool that saves a reusable multi-step procedure (skill) to persistent memory.
///
/// Saved skills are distinguished from regular memories by a `[SKILL] ` title prefix,
/// allowing the memory injection code to display them in a separate context block.
///
/// ## Two skill storage backends
///
/// Skills can reach the agent's context via two independent paths that are both
/// displayed under the same `[Relevant skills/procedures you've saved]` block:
///
/// 1. **`save_skill` tool** (this type) — agent-authored skills stored inside the
///    memvid memory store (BM25 + optional vector search).  The agent creates and
///    owns these entries; they travel with the user's memory database.
///
/// 2. **`SKILL.md` files** — manually maintained Markdown files placed by the user
///    in `<workspace>/.claude/skills/<name>/SKILL.md` (project-local) or
///    `<data_dir>/chatty/skills/<name>/SKILL.md` (global).  These are outside the
///    memory store and cannot be created or deleted by the agent.
///
/// Both paths produce `MemoryHit` objects and are merged before injection.  If the
/// same skill exists in both backends (e.g. the agent saved it *and* the user has a
/// handcrafted `SKILL.md`), both entries will appear — no automatic deduplication is
/// performed because they may contain different, complementary information.
#[derive(Clone)]
pub struct SaveSkillTool {
    memory_service: MemoryService,
    embedding_service: Option<EmbeddingService>,
}

impl SaveSkillTool {
    pub fn new(memory_service: MemoryService, embedding_service: Option<EmbeddingService>) -> Self {
        Self {
            memory_service,
            embedding_service,
        }
    }
}

impl Tool for SaveSkillTool {
    const NAME: &'static str = "save_skill";
    type Error = ToolError;
    type Args = SaveSkillArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "save_skill".to_string(),
            description: "Save a reusable multi-step procedure (skill) to persistent memory. \
                         Use this after successfully solving a new type of task to record the \
                         steps for future reuse. Saved skills are automatically surfaced at the \
                         start of future conversations when a similar task is detected. \
                         Good candidates: deployment workflows, data analysis pipelines, \
                         build/test procedures, API integration patterns. For Python-oriented \
                         skills, prefer recording shell steps that use `uv` for package management \
                         or `execute_code` when Docker-isolated execution is the better fit. \
                         Note: users can also provide skills as SKILL.md files in \
                         <workspace>/.claude/skills/<name>/ (project-local) or \
                         <data_dir>/chatty/skills/<name>/ (global). Both sources are merged \
                         into the same context block; calling save_skill is the agent-managed \
                         alternative for skills that should travel with the memory database."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short memorable name for this skill \
                                        (e.g., 'deploy to production', 'run CI checks', \
                                        'analyze csv data', 'set up postgres')."
                    },
                    "steps": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Ordered list of steps to perform this task. \
                                        Each step should be a clear, actionable instruction."
                    },
                    "description": {
                        "type": "string",
                        "description": "One-sentence description of when to use this skill \
                                        (e.g., 'Use when shipping a production release of chatty')."
                    }
                },
                "required": ["name", "steps", "description"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let steps_text = args
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| format!("{}. {}", i + 1, step))
            .collect::<Vec<_>>()
            .join("\n");

        let content = format!("Description: {}\n{}", args.description, steps_text);
        let title = format!("{}{}", SKILL_TITLE_PREFIX, args.name);

        if let Some(ref embed_svc) = self.embedding_service {
            match embed_svc.embed(&content).await {
                Ok(embedding) => {
                    self.memory_service
                        .remember_with_embedding(&content, embedding, Some(&title), &[])
                        .await
                        .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
                }
                Err(e) => {
                    warn!(error = ?e, "Embedding failed, falling back to BM25-only storage");
                    self.memory_service
                        .remember(&content, Some(&title), &[])
                        .await
                        .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
                }
            }
        } else {
            self.memory_service
                .remember(&content, Some(&title), &[])
                .await
                .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
        }

        Ok(format!("Skill saved: \"{}\"", args.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_prefix_is_correct() {
        assert_eq!(SKILL_TITLE_PREFIX, "[SKILL] ");
    }

    #[test]
    fn skill_title_starts_with_prefix() {
        let name = "deploy to production";
        let title = format!("{}{}", SKILL_TITLE_PREFIX, name);
        assert!(title.starts_with(SKILL_TITLE_PREFIX));
        assert_eq!(title, "[SKILL] deploy to production");
    }

    #[test]
    fn content_format() {
        let description = "Use when shipping a release";
        let steps = vec![
            "cargo build --release".to_string(),
            "scp binary to server".to_string(),
        ];
        let steps_text = steps
            .iter()
            .enumerate()
            .map(|(i, step)| format!("{}. {}", i + 1, step))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!("Description: {}\n{}", description, steps_text);
        assert_eq!(
            content,
            "Description: Use when shipping a release\n1. cargo build --release\n2. scp binary to server"
        );
    }
}
