use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

use super::remember_tool::MemoryToolError;
use crate::services::memory_service::{MemoryHit, MemoryService};

/// Arguments for the search_memory tool
#[derive(Deserialize, Serialize)]
pub struct SearchMemoryToolArgs {
    /// Natural language search query
    pub query: String,
    /// Maximum number of results to return (default: 5)
    #[serde(default)]
    pub top_k: Option<usize>,
}

/// Output from the search_memory tool
#[derive(Debug, Serialize)]
pub struct SearchMemoryToolOutput {
    pub results: Vec<MemoryHit>,
}

/// Tool that allows the agent to search its persistent memory.
///
/// Searches across all previously stored memories using full-text keyword search
/// (BM25 ranking). Queries match on exact words, so use specific keywords.
#[derive(Clone)]
pub struct SearchMemoryTool {
    memory_service: MemoryService,
}

impl SearchMemoryTool {
    pub fn new(memory_service: MemoryService) -> Self {
        Self { memory_service }
    }
}

impl Tool for SearchMemoryTool {
    const NAME: &'static str = "search_memory";
    type Error = MemoryToolError;
    type Args = SearchMemoryToolArgs;
    type Output = SearchMemoryToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "search_memory".to_string(),
            description: "Search persistent memory for previously stored information. \
                         Use this when you need to recall facts, decisions, user preferences, \
                         or context from past conversations. Uses keyword matching (BM25), \
                         so include specific words that are likely in the stored memory."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword query describing what you want to recall. \
                            Use concrete nouns and terms likely present in stored memories. \
                            Example: 'bananas fruit preference' rather than 'what foods does the user like'."
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return. Defaults to 5.",
                        "minimum": 1,
                        "maximum": 20
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let results = self
            .memory_service
            .search(&args.query, args.top_k)
            .await
            .map_err(|e| MemoryToolError::OperationFailed(e.to_string()))?;

        Ok(SearchMemoryToolOutput { results })
    }
}
