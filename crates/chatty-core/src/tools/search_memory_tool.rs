use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::services::memory_service::MemoryService;

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
    pub results: Vec<SearchMemoryResult>,
    pub total_found: usize,
}

/// A single memory search result
#[derive(Debug, Serialize)]
pub struct SearchMemoryResult {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub relevance_score: f32,
}

/// Error type for search_memory tool
#[derive(Debug, thiserror::Error)]
pub enum SearchMemoryToolError {
    #[error("Memory search error: {0}")]
    SearchError(String),
}

/// Tool that allows the agent to search its persistent memory.
///
/// Searches across all previously stored memories using combined vector similarity
/// and full-text search for high-quality recall.
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
    type Error = SearchMemoryToolError;
    type Args = SearchMemoryToolArgs;
    type Output = SearchMemoryToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "search_memory".to_string(),
            description: "Search persistent memory for previously stored information. \
                         Use this when you need to recall facts, decisions, user preferences, \
                         or context from past conversations. Returns the most relevant matches \
                         ranked by similarity."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language search query describing what you want to recall."
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
        info!(
            query = %args.query,
            top_k = args.top_k.unwrap_or(5),
            "Agent searching memory"
        );

        let hits = self
            .memory_service
            .search(&args.query, args.top_k)
            .await
            .map_err(|e| SearchMemoryToolError::SearchError(e.to_string()))?;

        let total_found = hits.len();

        let results = hits
            .into_iter()
            .map(|hit| SearchMemoryResult {
                text: hit.text,
                title: hit.title,
                relevance_score: hit.score,
            })
            .collect();

        Ok(SearchMemoryToolOutput {
            results,
            total_found,
        })
    }
}
