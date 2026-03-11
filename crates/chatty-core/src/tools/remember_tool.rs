use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::services::memory_service::MemoryService;

/// Arguments for the remember tool
#[derive(Deserialize, Serialize)]
pub struct RememberToolArgs {
    /// The information to remember
    pub content: String,
    /// Short title or label for the memory
    #[serde(default)]
    pub title: Option<String>,
    /// Key-value tags for categorization (e.g., {"project": "chatty", "topic": "architecture"})
    #[serde(default)]
    pub tags: Option<HashMap<String, String>>,
}

/// Shared error type for memory tools (remember + search_memory).
#[derive(Debug, thiserror::Error)]
pub enum MemoryToolError {
    #[error("Memory operation failed: {0}")]
    OperationFailed(String),
}

/// Tool that allows the agent to store important information in persistent memory.
///
/// Memories persist across conversations and can be recalled later via `search_memory`.
/// The agent decides what is worth remembering — user preferences, project context,
/// key decisions, or any fact that may be useful in future conversations.
#[derive(Clone)]
pub struct RememberTool {
    memory_service: MemoryService,
}

impl RememberTool {
    pub fn new(memory_service: MemoryService) -> Self {
        Self { memory_service }
    }
}

impl Tool for RememberTool {
    const NAME: &'static str = "remember";
    type Error = MemoryToolError;
    type Args = RememberToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "remember".to_string(),
            description:
                "Store important information in persistent memory for future conversations. \
                         Use this to save key facts, decisions, user preferences, project context, \
                         or anything the user might want you to recall later. Be selective — only \
                         store genuinely useful information that would be hard to re-derive."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The information to remember. IMPORTANT: Memory search uses keyword matching, \
                            so include synonyms, related terms, and category words to ensure future recall. \
                            Example: instead of just 'I like bananas', write 'User likes bananas. \
                            Categories: fruit, food preference, taste.' This ensures searching for \
                            'fruit' or 'food' will find this memory."
                    },
                    "title": {
                        "type": "string",
                        "description": "Short title or label for this memory (e.g., 'User prefers dark mode', 'Project uses PostgreSQL')."
                    },
                    "tags": {
                        "type": "object",
                        "description": "Optional key-value tags for categorization (e.g., {\"project\": \"chatty\", \"topic\": \"architecture\"}).",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "required": ["content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let tag_pairs: Vec<(String, String)> = args.tags.unwrap_or_default().into_iter().collect();

        let tag_refs: Vec<(&str, &str)> = tag_pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        self.memory_service
            .remember(&args.content, args.title.as_deref(), &tag_refs)
            .await
            .map_err(|e| MemoryToolError::OperationFailed(e.to_string()))?;

        Ok(format!(
            "Stored in memory: \"{}\"",
            args.title
                .as_deref()
                .unwrap_or(&args.content[..args.content.len().min(80)])
        ))
    }
}
