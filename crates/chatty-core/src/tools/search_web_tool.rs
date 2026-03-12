use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::settings::models::search_settings::SearchProvider;

/// Request timeout for search API calls
const SEARCH_TIMEOUT_SECS: u64 = 15;

/// Maximum snippet length per result (characters)
const MAX_SNIPPET_LENGTH: usize = 1000;

// ── Tool Args / Output ──────────────────────────────────────────────────────

/// Arguments for the search_web tool
#[derive(Deserialize, Serialize)]
pub struct SearchWebToolArgs {
    /// The search query
    pub query: String,
    /// Maximum number of results to return (overrides default)
    #[serde(default)]
    pub max_results: Option<usize>,
}

/// A single search result
#[derive(Debug, Serialize)]
pub struct SearchResult {
    /// Title of the search result
    pub title: String,
    /// URL of the search result
    pub url: String,
    /// Text snippet / description
    pub snippet: String,
}

/// Output from the search_web tool
#[derive(Debug, Serialize)]
pub struct SearchWebToolOutput {
    /// The original query
    pub query: String,
    /// Search results
    pub results: Vec<SearchResult>,
    /// Number of results returned
    pub result_count: usize,
}

/// Error type for the search_web tool
#[derive(Debug, thiserror::Error)]
pub enum SearchWebToolError {
    #[error("Search error: {0}")]
    SearchError(String),
}

// ── Tavily API types ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct TavilySearchRequest {
    query: String,
    max_results: usize,
    search_depth: String,
}

#[derive(Deserialize)]
struct TavilySearchResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

// ── Brave Search API types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct BraveSearchResponse {
    web: Option<BraveWebResults>,
}

#[derive(Deserialize)]
struct BraveWebResults {
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: String,
}

// ── Tool implementation ─────────────────────────────────────────────────────

/// Web search tool that queries Tavily or Brave Search APIs.
#[derive(Clone)]
pub struct SearchWebTool {
    client: reqwest::Client,
    provider: SearchProvider,
    api_key: String,
    default_max_results: usize,
}

impl SearchWebTool {
    pub fn new(provider: SearchProvider, api_key: String, default_max_results: usize) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(SEARCH_TIMEOUT_SECS))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .build()
            .expect("Failed to build HTTP client");
        Self {
            client,
            provider,
            api_key,
            default_max_results,
        }
    }

    async fn search_tavily(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchWebToolError> {
        let request = TavilySearchRequest {
            query: query.to_string(),
            max_results,
            search_depth: "basic".to_string(),
        };

        let response = self
            .client
            .post("https://api.tavily.com/search")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                SearchWebToolError::SearchError(format!("Tavily request failed: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(SearchWebToolError::SearchError(format!(
                "Tavily API returned {}: {}",
                status, body
            )));
        }

        let tavily_response: TavilySearchResponse = response.json().await.map_err(|e| {
            SearchWebToolError::SearchError(format!("Failed to parse Tavily response: {}", e))
        })?;

        Ok(tavily_response
            .results
            .into_iter()
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: truncate_snippet(&r.content),
            })
            .collect())
    }

    async fn search_brave(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, SearchWebToolError> {
        let response = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &max_results.to_string() as &str)])
            .send()
            .await
            .map_err(|e| SearchWebToolError::SearchError(format!("Brave request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(SearchWebToolError::SearchError(format!(
                "Brave Search API returned {}: {}",
                status, body
            )));
        }

        let brave_response: BraveSearchResponse = response.json().await.map_err(|e| {
            SearchWebToolError::SearchError(format!("Failed to parse Brave response: {}", e))
        })?;

        let results = brave_response
            .web
            .map(|w| {
                w.results
                    .into_iter()
                    .map(|r| SearchResult {
                        title: r.title,
                        url: r.url,
                        snippet: truncate_snippet(&r.description),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(results)
    }
}

impl Tool for SearchWebTool {
    const NAME: &'static str = "search_web";
    type Error = SearchWebToolError;
    type Args = SearchWebToolArgs;
    type Output = SearchWebToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "search_web".to_string(),
            description: "Search the web using a search engine and return relevant results. \
                         Use this to find up-to-date information, research topics, find documentation, \
                         or answer questions that require current web data."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query to look up on the web"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return. Defaults to the configured maximum."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(SearchWebToolError::SearchError(
                "Search query cannot be empty".to_string(),
            ));
        }

        let max_results = args.max_results.unwrap_or(self.default_max_results).clamp(1, 20);

        info!(
            query = %query,
            max_results = max_results,
            provider = %self.provider,
            "Performing web search"
        );

        let results = match self.provider {
            SearchProvider::Tavily => self.search_tavily(&query, max_results).await?,
            SearchProvider::Brave => self.search_brave(&query, max_results).await?,
        };

        let result_count = results.len();
        if result_count == 0 {
            warn!(query = %query, "Web search returned no results");
        }

        Ok(SearchWebToolOutput {
            query,
            results,
            result_count,
        })
    }
}

/// Truncate a snippet to a maximum length at a word boundary
fn truncate_snippet(s: &str) -> String {
    if s.len() <= MAX_SNIPPET_LENGTH {
        return s.to_string();
    }
    // Find last space before the limit
    let truncated = &s[..MAX_SNIPPET_LENGTH];
    if let Some(last_space) = truncated.rfind(' ') {
        format!("{}...", &s[..last_space])
    } else {
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_snippet_short() {
        let s = "Hello world";
        assert_eq!(truncate_snippet(s), "Hello world");
    }

    #[test]
    fn test_truncate_snippet_long() {
        let long = "a ".repeat(600); // 1200 chars
        let result = truncate_snippet(&long);
        assert!(result.len() <= MAX_SNIPPET_LENGTH + 5); // +5 for "..."
        assert!(result.ends_with("..."));
    }

    #[tokio::test]
    async fn test_search_web_tool_definition() {
        let tool = SearchWebTool::new(SearchProvider::Tavily, "test-key".into(), 5);
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "search_web");
        assert!(def.description.contains("Search the web"));
    }

    #[tokio::test]
    async fn test_search_web_tool_empty_query() {
        let tool = SearchWebTool::new(SearchProvider::Tavily, "test-key".into(), 5);
        let args = SearchWebToolArgs {
            query: "  ".to_string(),
            max_results: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }
}
