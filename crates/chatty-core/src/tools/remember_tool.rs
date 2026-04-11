use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;

use crate::services::embedding_service::EmbeddingService;
use crate::services::memory_service::MemoryService;
use crate::tools::ToolError;

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

/// Tool that allows the agent to store important information in persistent memory.
///
/// Memories persist across conversations and can be recalled later via `search_memory`.
/// The agent decides what is worth remembering — user preferences, project context,
/// key decisions, or any fact that may be useful in future conversations.
#[derive(Clone)]
pub struct RememberTool {
    memory_service: MemoryService,
    embedding_service: Option<EmbeddingService>,
}

impl RememberTool {
    pub fn new(memory_service: MemoryService, embedding_service: Option<EmbeddingService>) -> Self {
        Self {
            memory_service,
            embedding_service,
        }
    }
}

impl Tool for RememberTool {
    const NAME: &'static str = "remember";
    type Error = ToolError;
    type Args = RememberToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let content_description = if self.embedding_service.is_some() {
            "The information to remember. Write naturally — semantic search \
             handles conceptual matching automatically. No need to add extra \
             keywords or categories."
        } else {
            "The information to remember. IMPORTANT: Memory search uses keyword matching, \
             so include synonyms, related terms, and category words to ensure future recall. \
             Example: instead of just 'I like bananas', write 'User likes bananas. \
             Categories: fruit, food preference, taste.' This ensures searching for \
             'fruit' or 'food' will find this memory."
        };

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
                        "description": content_description
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

        // If embedding service is available, compute embedding and store with vector
        if let Some(ref embed_svc) = self.embedding_service {
            match embed_svc.embed(&args.content).await {
                Ok(embedding) => {
                    self.memory_service
                        .remember_with_embedding(
                            &args.content,
                            embedding,
                            args.title.as_deref(),
                            &tag_refs,
                        )
                        .await
                        .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
                }
                Err(e) => {
                    // Fall back to BM25-only storage if embedding fails
                    warn!(error = ?e, "Embedding failed, falling back to BM25-only storage");
                    self.memory_service
                        .remember(&args.content, args.title.as_deref(), &tag_refs)
                        .await
                        .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
                }
            }
        } else {
            self.memory_service
                .remember(&args.content, args.title.as_deref(), &tag_refs)
                .await
                .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
        }

        Ok(format!(
            "Stored in memory: \"{}\"",
            args.title
                .as_deref()
                .unwrap_or(&args.content[..args.content.len().min(80)])
        ))
    }
}
