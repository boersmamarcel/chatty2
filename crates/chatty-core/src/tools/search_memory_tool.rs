use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::warn;

use super::remember_tool::MemoryToolError;
use crate::services::embedding_service::EmbeddingService;
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
    embedding_service: Option<EmbeddingService>,
}

impl SearchMemoryTool {
    pub fn new(memory_service: MemoryService, embedding_service: Option<EmbeddingService>) -> Self {
        Self {
            memory_service,
            embedding_service,
        }
    }
}

impl Tool for SearchMemoryTool {
    const NAME: &'static str = "search_memory";
    type Error = MemoryToolError;
    type Args = SearchMemoryToolArgs;
    type Output = SearchMemoryToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let (description, query_description) = if self.embedding_service.is_some() {
            (
                "Search persistent memory for previously stored information. \
                 Uses hybrid search: keyword matching (BM25) + semantic similarity. \
                 You can use natural language queries — searching 'fruits' will find \
                 memories about bananas, apples, etc.",
                "Natural language query describing what you want to recall. \
                 Both specific keywords and conceptual descriptions work. \
                 Example: 'food preferences' will find memories about specific foods.",
            )
        } else {
            (
                "Search persistent memory for previously stored information. \
                 Use this when you need to recall facts, decisions, user preferences, \
                 or context from past conversations. Uses keyword matching (BM25), \
                 so include specific words that are likely in the stored memory.",
                "Keyword query describing what you want to recall. \
                 Use concrete nouns and terms likely present in stored memories. \
                 Example: 'bananas fruit preference' rather than 'what foods does the user like'.",
            )
        };

        ToolDefinition {
            name: "search_memory".to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": query_description
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
        // BM25 lexical search (always runs)
        let lex_results = self
            .memory_service
            .search(&args.query, args.top_k)
            .await
            .map_err(|e| MemoryToolError::OperationFailed(e.to_string()))?;

        // Vector search (if embedding service is available)
        let vec_results = if let Some(ref embed_svc) = self.embedding_service {
            match embed_svc.embed(&args.query).await {
                Ok(embedding) => self
                    .memory_service
                    .search_vec(embedding, args.top_k)
                    .await
                    .unwrap_or_else(|e| {
                        warn!(error = ?e, "Vector search failed, using BM25 only");
                        Vec::new()
                    }),
                Err(e) => {
                    warn!(error = ?e, "Query embedding failed, using BM25 only");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let results = merge_search_results(lex_results, vec_results, args.top_k.unwrap_or(5));

        Ok(SearchMemoryToolOutput { results })
    }
}

/// Merge BM25 and vector search results, deduplicating by text content.
///
/// For duplicates (same text from both sources), keeps the higher score.
/// Returns up to `limit` results sorted by descending score.
pub fn merge_search_results(
    lex_results: Vec<MemoryHit>,
    vec_results: Vec<MemoryHit>,
    limit: usize,
) -> Vec<MemoryHit> {
    if vec_results.is_empty() {
        return lex_results;
    }
    if lex_results.is_empty() {
        return vec_results.into_iter().take(limit).collect();
    }

    // Deduplicate by text content, keeping the higher score
    let mut seen_texts = HashSet::new();
    let mut merged: Vec<MemoryHit> = Vec::with_capacity(lex_results.len() + vec_results.len());

    // Add all results, tracking seen texts
    for hit in lex_results.into_iter().chain(vec_results.into_iter()) {
        // Use first 200 chars as dedup key to avoid expensive full-text comparison
        let key = hit.text.chars().take(200).collect::<String>();
        if seen_texts.contains(&key) {
            // Update score if this duplicate has a higher score
            if let Some(existing) = merged
                .iter_mut()
                .find(|h| h.text.chars().take(200).collect::<String>() == key)
                && hit.score > existing.score
            {
                existing.score = hit.score;
            }
        } else {
            seen_texts.insert(key);
            merged.push(hit);
        }
    }

    // Sort by descending score and take top `limit`
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(limit);
    merged
}
