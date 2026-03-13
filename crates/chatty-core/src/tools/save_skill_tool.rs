use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

use crate::services::memory_service::MemoryService;
use crate::tools::remember_tool::MemoryToolError;

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
#[derive(Clone)]
pub struct SaveSkillTool {
    memory_service: MemoryService,
}

impl SaveSkillTool {
    pub fn new(memory_service: MemoryService) -> Self {
        Self { memory_service }
    }
}

impl Tool for SaveSkillTool {
    const NAME: &'static str = "save_skill";
    type Error = MemoryToolError;
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
                         build/test procedures, API integration patterns."
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

        self.memory_service
            .remember(&content, Some(&title), &[("skill", "true")])
            .await
            .map_err(|e| MemoryToolError::OperationFailed(e.to_string()))?;

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
