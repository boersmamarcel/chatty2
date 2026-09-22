use base64::Engine as _;
#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::settings::models::search_settings::SearchProvider;
use crate::tools::ToolError;
use crate::tools::html_entities::decode_html_entities;

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

/// Web search tool that queries Tavily or Brave Search APIs,
/// with a DuckDuckGo fetch-based fallback when no API key is configured.
#[derive(Clone)]
pub struct SearchWebTool {
    client: reqwest::Client,
    /// None means fallback mode (use DuckDuckGo lite HTML scraping)
    provider: Option<SearchProvider>,
    /// None means fallback mode (no API key configured)
    api_key: Option<String>,
    default_max_results: usize,
}

impl SearchWebTool {
    /// Create a search tool backed by a configured API provider (Tavily or Brave).
    pub fn new(provider: SearchProvider, api_key: String, default_max_results: usize) -> Self {
        let client = crate::services::http_client::default_client(SEARCH_TIMEOUT_SECS);
        Self {
            client,
            provider: Some(provider),
            api_key: Some(api_key),
            default_max_results,
        }
    }

    /// Create a search tool in fallback mode: uses DuckDuckGo lite HTML scraping.
    /// This requires no API key and provides basic web search capability.
    pub fn new_fallback(default_max_results: usize) -> Self {
        let client = crate::services::http_client::browser_client(SEARCH_TIMEOUT_SECS);
        Self {
            client,
            provider: None,
            api_key: None,
            default_max_results,
        }
    }

    async fn search_tavily(
        &self,
        query: &str,
        max_results: usize,
        api_key: &str,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let request = TavilySearchRequest {
            query: query.to_string(),
            max_results,
            search_depth: "basic".to_string(),
        };

        let response = self
            .client
            .post("https://api.tavily.com/search")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Tavily request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(ToolError::OperationFailed(format!(
                "Tavily API returned {}: {}",
                status, body
            )));
        }

        let tavily_response: TavilySearchResponse = response.json().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to parse Tavily response: {}", e))
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
        api_key: &str,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let response = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &max_results.to_string() as &str)])
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Brave request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(ToolError::OperationFailed(format!(
                "Brave Search API returned {}: {}",
                status, body
            )));
        }

        let brave_response: BraveSearchResponse = response.json().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to parse Brave response: {}", e))
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

    /// Fallback search with no API key: tries Bing HTML first (works from
    /// this network as of 2026-09-21), then DuckDuckGo lite. DuckDuckGo now
    /// answers most queries with a 202 "anomaly" challenge page instead of
    /// real results (AGE-495), so it is kept only as a second attempt.
    async fn search_web_fallback(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, ToolError> {
        match self.search_bing_fallback(query, max_results).await {
            Ok(results) => Ok(results),
            Err(bing_error) => {
                warn!(query = %query, error = %bing_error, "Bing fallback failed; trying DuckDuckGo lite");
                self.search_duckduckgo_fallback(query, max_results)
                    .await
                    .map_err(|ddg_error| {
                        ToolError::OperationFailed(format!(
                            "web search is unavailable: Bing failed ({bing_error}); \
                             DuckDuckGo failed ({ddg_error})"
                        ))
                    })
            }
        }
    }

    /// Fetches `https://www.bing.com/search?q=<query>` and parses `b_algo` result blocks.
    async fn search_bing_fallback(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let response = self
            .client
            .get("https://www.bing.com/search")
            .query(&bing_query_params(query))
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Bing request failed: {}", e)))?;

        if response.status() != reqwest::StatusCode::OK {
            return Err(ToolError::OperationFailed(format!(
                "Bing returned HTTP {}",
                response.status()
            )));
        }

        let html = response.text().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to read Bing response: {}", e))
        })?;

        let results = parse_bing_results(&html, max_results);
        // Bing answers some clients with decoy results — well-formed `b_algo`
        // blocks about something else entirely, re-rolled on every request
        // (AGE-506). Like the DDG challenge page below, that is an
        // unavailable backend, not an answer, so say so instead of handing
        // the model a confident-looking list of unrelated links.
        if !results.is_empty() && !results_match_query(query, &results) {
            return Err(ToolError::OperationFailed(
                "Bing returned results unrelated to the query (anti-scraping decoy page); \
                 web search is unavailable"
                    .to_string(),
            ));
        }
        Ok(results)
    }

    /// Fallback search using DuckDuckGo lite (no API key required).
    /// Fetches `https://lite.duckduckgo.com/lite/?q=<query>` and parses the HTML results.
    async fn search_duckduckgo_fallback(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let response = self
            .client
            .get("https://lite.duckduckgo.com/lite/")
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("DuckDuckGo request failed: {}", e)))?;

        // DDG now answers most queries with a 202 "anomaly" bot-challenge
        // page rather than the 200 the lite HTML scraper expects; treat
        // anything but a plain 200 as unavailable rather than parsing it as
        // an (empty) result page (AGE-495).
        if response.status() != reqwest::StatusCode::OK {
            return Err(ToolError::OperationFailed(format!(
                "DuckDuckGo returned HTTP {} (bot challenge or block); web search is unavailable",
                response.status()
            )));
        }

        let html = response.text().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to read DuckDuckGo response: {}", e))
        })?;

        let results = parse_ddg_lite_results(&html, max_results);
        if results.is_empty() && looks_like_ddg_challenge_page(&html) {
            return Err(ToolError::OperationFailed(
                "DuckDuckGo served a bot challenge page; web search is unavailable".to_string(),
            ));
        }
        Ok(results)
    }
}

impl Tool for SearchWebTool {
    const NAME: &'static str = "search_web";
    type Error = ToolError;
    type Args = SearchWebToolArgs;
    type Output = SearchWebToolOutput;

    fn description(&self) -> String {
        "Search the web and return relevant results. \
                         Use this to find up-to-date information, research topics, find documentation, \
                         or answer questions that require current web data."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(ToolError::OperationFailed(
                "Search query cannot be empty".to_string(),
            ));
        }

        let max_results = args
            .max_results
            .unwrap_or(self.default_max_results)
            .clamp(1, 20);

        let results = match (&self.provider, &self.api_key) {
            (Some(SearchProvider::Tavily), Some(key)) => {
                info!(query = %query, max_results, provider = "tavily", "Performing web search");
                self.search_tavily(&query, max_results, key).await?
            }
            (Some(SearchProvider::Brave), Some(key)) => {
                info!(query = %query, max_results, provider = "brave", "Performing web search");
                self.search_brave(&query, max_results, key).await?
            }
            _ => {
                info!(query = %query, max_results, provider = "keyless-fallback", "Performing web search");
                self.search_web_fallback(&query, max_results).await?
            }
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

/// Parse search results from DuckDuckGo lite HTML.
///
/// DDG lite returns a simple table-based HTML page. Each result has:
/// - A `<a class="result-link">` anchor with href and title text
/// - A `<td class="result-snippet">` cell with the snippet text
fn parse_ddg_lite_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Extract all result links: <a class="result-link" href="...">Title</a>
    let mut pos = 0;
    while results.len() < max_results {
        // Find next result-link anchor
        let link_marker = "class=\"result-link\"";
        let Some(link_start) = html[pos..].find(link_marker) else {
            break;
        };
        let link_start = pos + link_start;

        // Find the href attribute within this anchor tag regardless of
        // attribute ordering, e.g. either `class=... href=...` or `href=... class=...`.
        let tag_start = html[..link_start].rfind('<').unwrap_or(link_start);
        let Some(_tag_end_offset) = html[link_start..].find('>') else {
            pos = link_start + link_marker.len();
            continue;
        };

        // Search for href in the full anchor tag (up to >) so attribute order doesn't matter
        let tag_end = html[tag_start..]
            .find('>')
            .map(|p| tag_start + p)
            .unwrap_or(link_start + link_marker.len());

        // Extract href value
        let href_marker = "href=\"";
        let url = if let Some(href_pos) = html[tag_start..tag_end].find(href_marker) {
            let href_value_start = tag_start + href_pos + href_marker.len();
            if let Some(href_end) = html[href_value_start..].find('"') {
                html[href_value_start..href_value_start + href_end].to_string()
            } else {
                pos = link_start + link_marker.len();
                continue;
            }
        } else {
            pos = link_start + link_marker.len();
            continue;
        };

        // Skip non-http URLs (DDG internal links)
        if !url.starts_with("http") {
            pos = link_start + link_marker.len();
            continue;
        }

        // Extract anchor text (title) — between > and </a>
        let anchor_close = html[link_start..].find('>');
        let title = if let Some(close_pos) = anchor_close {
            let text_start = link_start + close_pos + 1;
            if let Some(end_anchor) = html[text_start..].find("</a>") {
                strip_html_tags(&html[text_start..text_start + end_anchor])
                    .trim()
                    .to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Find the next result-snippet after this link
        let snippet_marker = "class=\"result-snippet\"";
        let snippet = if let Some(snip_start) = html[link_start..].find(snippet_marker) {
            let snip_start = link_start + snip_start;
            if let Some(td_close) = html[snip_start..].find('>') {
                let text_start = snip_start + td_close + 1;
                if let Some(td_end) = html[text_start..].find("</td>") {
                    let raw = strip_html_tags(&html[text_start..text_start + td_end]);
                    truncate_snippet(raw.trim())
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !url.is_empty() && !title.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        pos = link_start + link_marker.len();
    }

    results
}

/// Whether a DuckDuckGo lite response with no parsed results looks like the
/// JS "anomaly"/"challenge" bot-check page rather than a genuine zero-result
/// search (AGE-495).
fn looks_like_ddg_challenge_page(html: &str) -> bool {
    let lower = html.to_lowercase();
    lower.contains("anomaly") || lower.contains("challenge")
}

/// Query parameters for a Bing search request.
///
/// `setlang`/`cc` ask Bing for English results/UI and US ranking instead of
/// guessing locale from the requester's IP (AGE-508); complementary to the
/// `Accept-Language` header the shared HTTP client builders now send
/// (`services::http_client`).
fn bing_query_params(query: &str) -> [(&str, &str); 3] {
    [("q", query), ("setlang", "en"), ("cc", "US")]
}

/// Parse search results from a Bing HTML results page.
///
/// Each organic result is a `<li class="b_algo">` block containing a
/// `<h2><a href="...">Title</a></h2>` and a `<div class="b_caption"><p>snippet</p>`.
fn parse_bing_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut pos = 0;

    while results.len() < max_results {
        let block_marker = "class=\"b_algo\"";
        let Some(block_start) = html[pos..].find(block_marker) else {
            break;
        };
        let block_start = pos + block_start;
        // Bound this block by the start of the next one (or end of document)
        // so title/snippet extraction doesn't bleed into later results.
        let block_end = html[block_start + block_marker.len()..]
            .find(block_marker)
            .map(|p| block_start + block_marker.len() + p)
            .unwrap_or(html.len());
        let block = &html[block_start..block_end];
        pos = block_end;

        let Some(h2_start) = block.find("<h2") else {
            continue;
        };
        let after_h2 = &block[h2_start..];

        let href_marker = "href=\"";
        let Some(href_pos) = after_h2.find(href_marker) else {
            continue;
        };
        let href_value_start = href_pos + href_marker.len();
        let Some(href_end) = after_h2[href_value_start..].find('"') else {
            continue;
        };
        let url = decode_bing_redirect(&after_h2[href_value_start..href_value_start + href_end]);
        if !url.starts_with("http") {
            continue;
        }

        let title = extract_anchor_title(after_h2).unwrap_or_default();
        let snippet = extract_bing_caption_snippet(block).unwrap_or_default();

        if !url.is_empty() && !title.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    results
}

/// Resolve a Bing result href to the page it actually points at.
///
/// Bing wraps every organic result in a tracking redirect shaped
/// `https://www.bing.com/ck/a?...&u=a1<base64url of the real URL>&ntb=1`, and
/// the source page carries it HTML-escaped (`&amp;`). Without this the model
/// only ever sees the redirect and has to guess URLs from titles (AGE-506).
///
/// Best effort: anything that is not that exact shape — a direct link, a
/// changed redirect format, a payload that is not base64url of a URL — falls
/// back to the href as found rather than failing the search.
fn decode_bing_redirect(href: &str) -> String {
    decode_bing_redirect_inner(href).unwrap_or_else(|| href.to_string())
}

fn decode_bing_redirect_inner(href: &str) -> Option<String> {
    let unescaped = decode_html_entities(href);
    if !unescaped.contains("bing.com/ck/a") {
        return None;
    }
    let query = unescaped.split_once('?')?.1;
    let u = query
        .split('&')
        .find_map(|param| param.strip_prefix("u="))?;
    // Bing prefixes the base64url payload with a two-character scheme tag.
    let payload = u.strip_prefix("a1").unwrap_or(u);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let url = String::from_utf8(bytes).ok()?;
    url.starts_with("http").then_some(url)
}

/// Whether any parsed result plausibly belongs to `query`: one content term
/// (a word of 4+ characters) of the query occurring in some result's title,
/// snippet or URL is enough. A query with no such term (e.g. "who won") can't
/// be judged this way and passes.
fn results_match_query(query: &str, results: &[SearchResult]) -> bool {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .map(str::to_lowercase)
        .collect();
    if terms.is_empty() {
        return true;
    }
    results.iter().any(|result| {
        let haystack = format!("{} {} {}", result.title, result.snippet, result.url).to_lowercase();
        terms.iter().any(|term| haystack.contains(term))
    })
}

/// Extract the anchor text of the first `<a ...>...</a>` in `html`, with
/// tags stripped and whitespace trimmed.
fn extract_anchor_title(html: &str) -> Option<String> {
    let tag_close = html.find('>')?;
    let text_start = tag_close + 1;
    let end_anchor = html[text_start..].find("</a>")?;
    Some(
        strip_html_tags(&html[text_start..text_start + end_anchor])
            .trim()
            .to_string(),
    )
}

/// Extract the snippet text from a Bing result block's `<div class="b_caption"><p>...</p>`.
fn extract_bing_caption_snippet(block: &str) -> Option<String> {
    let caption_start = block.find("b_caption")?;
    let p_start = caption_start + block[caption_start..].find("<p")?;
    let text_start = p_start + block[p_start..].find('>')? + 1;
    let p_end = block[text_start..].find("</p>")?;
    Some(truncate_snippet(
        strip_html_tags(&block[text_start..text_start + p_end]).trim(),
    ))
}

/// Lightweight HTML tag stripper that decodes common entities.
/// Used for short search-result snippets where script/style blocks and
/// block-level whitespace handling are unnecessary. For full HTML-to-text
/// conversion see `fetch_tool::html_to_text`.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    decode_html_entities(&result)
}

/// Truncate a snippet to a maximum length at a word boundary
fn truncate_snippet(s: &str) -> String {
    if s.chars().count() <= MAX_SNIPPET_LENGTH {
        return s.to_string();
    }

    let truncated = s.chars().take(MAX_SNIPPET_LENGTH).collect::<String>();
    if let Some(last_space) = truncated.rfind(' ') {
        format!("{}...", &truncated[..last_space])
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

    #[test]
    fn test_truncate_snippet_multibyte_boundary() {
        let long = format!("{}📈 trailing text", "a".repeat(MAX_SNIPPET_LENGTH - 1));
        let result = truncate_snippet(&long);
        assert!(result.ends_with("..."));
        assert!(result.contains('📈'));
        assert!(result.chars().count() <= MAX_SNIPPET_LENGTH + 3);
    }

    #[tokio::test]
    async fn test_search_web_tool_definition() {
        let tool = SearchWebTool::new(SearchProvider::Tavily, "test-key".into(), 5);
        let def = tool_definition(&tool);
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
        let result = tool.call(&mut ToolContext::new(), args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fallback_tool_definition() {
        let tool = SearchWebTool::new_fallback(5);
        let def = tool_definition(&tool);
        assert_eq!(def.name, "search_web");
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>Hello</b> &amp; world"), "Hello & world");
        assert_eq!(strip_html_tags("plain text"), "plain text");
    }

    #[test]
    fn test_parse_ddg_lite_no_results() {
        let results = parse_ddg_lite_results("<html><body>No results</body></html>", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_ddg_lite_basic() {
        let html = r#"<html><body>
            <a class="result-link" href="https://example.com">Example Title</a>
            <td class="result-snippet">Some snippet text here</td>
        </body></html>"#;
        let results = parse_ddg_lite_results(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com");
        assert_eq!(results[0].title, "Example Title");
        assert_eq!(results[0].snippet, "Some snippet text here");
    }

    /// AGE-495: DDG's 202 bot-check page has no `result-link` anchors, so it
    /// parses to zero results just like a genuine empty search — the
    /// "anomaly"/"challenge" markers are what tell the two apart.
    #[test]
    fn test_ddg_challenge_page_is_detected() {
        let challenge_page = r#"<html><body>
            <div id="anomaly-modal">
                Unfortunately, bots use DuckDuckGo too. Please complete the challenge below.
            </div>
        </body></html>"#;
        assert!(parse_ddg_lite_results(challenge_page, 5).is_empty());
        assert!(looks_like_ddg_challenge_page(challenge_page));
    }

    #[test]
    fn test_ddg_genuine_no_results_is_not_a_challenge_page() {
        let no_results_page = "<html><body>No results.</body></html>";
        assert!(parse_ddg_lite_results(no_results_page, 5).is_empty());
        assert!(!looks_like_ddg_challenge_page(no_results_page));
    }

    /// AGE-508: the Bing request asks for English results / US ranking
    /// (`setlang`/`cc`) instead of letting Bing guess locale from IP, and
    /// still carries the query text — checked on the actual built request
    /// URL, not just the parameter tuple, since `RequestBuilder::query`
    /// could in principle be wired up wrong.
    #[test]
    fn test_bing_request_includes_setlang_and_cc() {
        assert_eq!(
            bing_query_params("rust lang"),
            [("q", "rust lang"), ("setlang", "en"), ("cc", "US")]
        );

        let client = crate::services::http_client::browser_client(1);
        let request = client
            .get("https://www.bing.com/search")
            .query(&bing_query_params("rust lang"))
            .build()
            .unwrap();
        let query = request.url().query().unwrap_or("");
        assert!(
            query.contains("q=rust+lang") || query.contains("q=rust%20lang"),
            "got {query}"
        );
        assert!(query.contains("setlang=en"), "got {query}");
        assert!(query.contains("cc=US"), "got {query}");
    }

    /// AGE-495: Bing HTML fallback, kept working when DDG serves a challenge.
    #[test]
    fn test_parse_bing_results_basic() {
        let html = r#"<html><body>
            <ol id="b_results">
                <li class="b_algo">
                    <h2><a href="https://example.com/one">First Result</a></h2>
                    <div class="b_caption"><p>First snippet text.</p></div>
                </li>
                <li class="b_algo">
                    <h2><a href="https://example.com/two">Second Result</a></h2>
                    <div class="b_caption"><p>Second snippet text.</p></div>
                </li>
            </ol>
        </body></html>"#;
        let results = parse_bing_results(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://example.com/one");
        assert_eq!(results[0].title, "First Result");
        assert_eq!(results[0].snippet, "First snippet text.");
        assert_eq!(results[1].url, "https://example.com/two");
        assert_eq!(results[1].title, "Second Result");
    }

    /// AGE-506 (1): every real Bing href is an HTML-escaped `ck/a` tracking
    /// redirect; the model needs the destination, not the redirect.
    #[test]
    fn test_parse_bing_results_decodes_redirect_href() {
        let html = r#"<li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&amp;&amp;p=abc123&amp;u=a1aHR0cHM6Ly93d3cuYnJpdGlzaG11c2V1bS5vcmcvY29sbGVjdGlvbg&amp;ntb=1">The British Museum</a></h2>
            <div class="b_caption"><p>Collection of the British Museum.</p></div></li>"#;
        let results = parse_bing_results(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://www.britishmuseum.org/collection");
    }

    /// The exact payload shape observed in the wild (AGE-506 evidence).
    #[test]
    fn test_decode_bing_redirect_matches_observed_payload() {
        let href =
            "https://www.bing.com/ck/a?u=a1aHR0cHM6Ly9lbi53aWtpcGVkaWEub3JnL3dpa2kvVGhl&ntb=1";
        assert_eq!(
            decode_bing_redirect(href),
            "https://en.wikipedia.org/wiki/The"
        );
    }

    #[test]
    fn test_decode_bing_redirect_leaves_direct_links_alone() {
        let href = "https://example.com/one";
        assert_eq!(decode_bing_redirect(href), href);
    }

    /// Best effort: a redirect we can't decode must degrade to the raw href,
    /// never fail the whole search.
    #[test]
    fn test_decode_bing_redirect_falls_back_on_malformed_payload() {
        for href in [
            "https://www.bing.com/ck/a?p=abc&ntb=1", // no u= at all
            "https://www.bing.com/ck/a?u=a1!!!not-base64!!!", // undecodable
            "https://www.bing.com/ck/a?u=a1bm90IGEgdXJs", // decodes to "not a url"
            "https://www.bing.com/ck/a",             // no query string
        ] {
            assert_eq!(decode_bing_redirect(href), href, "href: {href}");
        }
    }

    /// AGE-506 (2): decoy results parse perfectly but have nothing to do with
    /// the query, the same way the DDG challenge page parses to nothing.
    #[test]
    fn test_bing_decoy_results_are_detected() {
        let decoys = vec![
            SearchResult {
                title: "British Airways | Book flights".to_string(),
                url: "https://www.britishairways.com/".to_string(),
                snippet: "Find cheap flights and book online.".to_string(),
            },
            SearchResult {
                title: "Quiz Widget".to_string(),
                url: "https://quizwidget.example/".to_string(),
                snippet: "Add a quiz to your page.".to_string(),
            },
        ];
        assert!(!results_match_query("Virtue restaurant Chicago", &decoys));
    }

    #[test]
    fn test_bing_relevant_results_pass_the_guard() {
        let real = vec![SearchResult {
            title: "The British Museum".to_string(),
            url: "https://www.britishmuseum.org/collection".to_string(),
            snippet: "Explore the collection.".to_string(),
        }];
        assert!(results_match_query("British Museum opening hours", &real));
        // A term may match through the decoded URL or the snippet alone.
        assert!(results_match_query("britishmuseum collection", &real));
    }

    #[test]
    fn test_query_without_content_terms_is_not_judged() {
        let results = vec![SearchResult {
            title: "Quiz Widget".to_string(),
            url: "https://quizwidget.example/".to_string(),
            snippet: "Add a quiz to your page.".to_string(),
        }];
        assert!(results_match_query("who won", &results));
    }

    /// AGE-506 (3): real Bing output carries numeric references and accented
    /// Latin entities the old six-entry table passed through unrendered.
    #[test]
    fn test_strip_html_tags_decodes_numeric_and_named_entities() {
        assert_eq!(
            strip_html_tags("caf&#233; &#0183; r&eacute;sum&eacute;"),
            "café · résumé"
        );
        assert_eq!(strip_html_tags("&#x27;quoted&#x27;"), "'quoted'");
        assert_eq!(
            strip_html_tags("2024 &ndash; 2025 &hellip;"),
            "2024 – 2025 …"
        );
        assert_eq!(
            strip_html_tags("&copy; M&uuml;ller &amp; S&oslash;n"),
            "© Müller & Søn"
        );
        assert_eq!(strip_html_tags("a&nbsp;b"), "a b");
    }

    #[test]
    fn test_decode_html_entities_leaves_unknown_and_bare_ampersands() {
        assert_eq!(decode_html_entities("AT&T and Q&A"), "AT&T and Q&A");
        assert_eq!(decode_html_entities("&notareal;"), "&notareal;");
        // Single pass: an escaped entity stays escaped text.
        assert_eq!(decode_html_entities("&amp;lt;"), "&lt;");
    }

    #[test]
    fn test_parse_bing_results_respects_max_results() {
        let html = r#"<li class="b_algo"><h2><a href="https://a.com">A</a></h2><div class="b_caption"><p>a</p></div></li>
            <li class="b_algo"><h2><a href="https://b.com">B</a></h2><div class="b_caption"><p>b</p></div></li>"#;
        let results = parse_bing_results(html, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://a.com");
    }
}
