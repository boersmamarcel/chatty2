use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Default maximum response length in characters
const DEFAULT_MAX_LENGTH: usize = 50_000;

/// Request timeout in seconds
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Arguments for the fetch tool
#[derive(Deserialize, Serialize)]
pub struct FetchToolArgs {
    /// The URL to fetch
    pub url: String,
    /// Maximum length of the returned content in characters (default: 50000)
    #[serde(default)]
    pub max_length: Option<usize>,
}

/// Output from the fetch tool
#[derive(Debug, Serialize)]
pub struct FetchToolOutput {
    /// HTTP status code
    pub status: u16,
    /// The readable text content of the response
    pub content: String,
    /// The content type of the response
    pub content_type: String,
    /// Whether the content was truncated due to max_length
    pub truncated: bool,
}

/// Error type for fetch tool
#[derive(Debug, thiserror::Error)]
pub enum FetchToolError {
    #[error("Fetch error: {0}")]
    FetchError(String),
}

/// Native fetch tool that provides read-only HTTP GET access to web content.
///
/// Converts HTML responses to readable plain text, preserving non-HTML content as-is.
/// Enforces timeouts, size limits, and HTTPS preference for safety.
#[derive(Clone)]
pub struct FetchTool;

impl FetchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for FetchTool {
    const NAME: &'static str = "fetch";
    type Error = FetchToolError;
    type Args = FetchToolArgs;
    type Output = FetchToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "fetch".to_string(),
            description: "Fetch a URL and return its content as readable text. \
                         HTML pages are automatically converted to plain text for readability. \
                         Only performs GET requests (read-only). \
                         Use this to look up documentation, read web pages, or fetch API responses."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch. HTTPS is preferred for security."
                    },
                    "max_length": {
                        "type": "integer",
                        "description": "Maximum length of returned content in characters. Defaults to 50000."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let url = args.url.trim().to_string();
        let max_length = args.max_length.unwrap_or(DEFAULT_MAX_LENGTH);

        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(FetchToolError::FetchError(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        info!(url = %url, max_length = max_length, "Fetching URL");

        // Build HTTP client with timeout
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .build()
            .map_err(|e| {
                FetchToolError::FetchError(format!("Failed to build HTTP client: {}", e))
            })?;

        // Perform GET request
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| FetchToolError::FetchError(format!("Request failed: {}", e)))?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        info!(status = status, content_type = %content_type, "Received response");

        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            let truncated = body.len() > max_length;
            let body = if truncated {
                truncate_at_char_boundary(&body, max_length)
            } else {
                body
            };
            return Ok(FetchToolOutput {
                status,
                content: body,
                content_type,
                truncated,
            });
        }

        // Read body text
        let body = response.text().await.map_err(|e| {
            FetchToolError::FetchError(format!("Failed to read response body: {}", e))
        })?;

        // Convert HTML to readable text if appropriate
        let is_html = content_type.contains("text/html") || looks_like_html(&body);
        let content = if is_html { html_to_text(&body) } else { body };

        // Truncate if needed
        let truncated = content.len() > max_length;
        let content = if truncated {
            warn!(
                original_len = content.len(),
                max_length = max_length,
                "Truncating response content"
            );
            truncate_at_char_boundary(&content, max_length)
        } else {
            content
        };

        Ok(FetchToolOutput {
            status,
            content,
            content_type,
            truncated,
        })
    }
}

/// Simple heuristic to detect HTML content when content-type is missing or ambiguous
fn looks_like_html(body: &str) -> bool {
    let trimmed = body.trim_start();
    trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
}

/// Truncate a string at a char boundary, not in the middle of a multi-byte character
fn truncate_at_char_boundary(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    // Find the last char boundary at or before max_len
    let mut end = max_len;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    let mut result = s[..end].to_string();
    result.push_str("\n\n[Content truncated]");
    result
}

/// Convert HTML to readable plain text.
///
/// Strips tags and extracts readable content. Uses a simple approach
/// that handles common HTML elements without requiring a heavyweight parser.
fn html_to_text(html: &str) -> String {
    let mut result = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_whitespace = false;
    let mut tag_name = String::new();
    let mut collecting_tag_name = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            collecting_tag_name = true;
            tag_name.clear();
            continue;
        }

        if in_tag {
            if collecting_tag_name {
                if ch.is_alphanumeric() || ch == '/' {
                    tag_name.push(ch.to_ascii_lowercase());
                } else {
                    collecting_tag_name = false;
                }
            }
            if ch == '>' {
                in_tag = false;
                collecting_tag_name = false;

                // Track script/style blocks to skip their content
                if tag_name == "script" {
                    in_script = true;
                } else if tag_name == "/script" {
                    in_script = false;
                } else if tag_name == "style" {
                    in_style = true;
                } else if tag_name == "/style" {
                    in_style = false;
                }

                // Add line breaks for block-level elements
                let is_block = matches!(
                    tag_name.trim_start_matches('/'),
                    "p" | "div"
                        | "br"
                        | "br/"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "li"
                        | "tr"
                        | "blockquote"
                        | "pre"
                        | "hr"
                        | "header"
                        | "footer"
                        | "section"
                        | "article"
                        | "nav"
                        | "main"
                );
                if is_block && !result.ends_with('\n') {
                    result.push('\n');
                    last_was_whitespace = true;
                }
            }
            continue;
        }

        // Skip content inside script and style blocks
        if in_script || in_style {
            continue;
        }

        // Normalize whitespace
        if ch.is_whitespace() {
            if !last_was_whitespace {
                result.push(' ');
                last_was_whitespace = true;
            }
        } else {
            result.push(ch);
            last_was_whitespace = false;
        }
    }

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Clean up excessive newlines
    let mut cleaned = String::with_capacity(result.len());
    let mut consecutive_newlines = 0;
    for ch in result.chars() {
        if ch == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                cleaned.push(ch);
            }
        } else {
            consecutive_newlines = 0;
            cleaned.push(ch);
        }
    }

    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_text_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_html_to_text_strips_script() {
        let html = "<p>Before</p><script>alert('xss')</script><p>After</p>";
        let text = html_to_text(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_html_to_text_strips_style() {
        let html = "<p>Text</p><style>body { color: red; }</style><p>More</p>";
        let text = html_to_text(html);
        assert!(text.contains("Text"));
        assert!(text.contains("More"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn test_html_to_text_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D &quot;E&quot;</p>";
        let text = html_to_text(html);
        assert!(text.contains("A & B < C > D \"E\""));
    }

    #[test]
    fn test_looks_like_html() {
        assert!(looks_like_html("<!DOCTYPE html><html>"));
        assert!(looks_like_html("  <!doctype html>"));
        assert!(looks_like_html("<html><head>"));
        assert!(!looks_like_html("{\"key\": \"value\"}"));
        assert!(!looks_like_html("plain text"));
    }

    #[test]
    fn test_truncate_at_char_boundary() {
        let text = "Hello, World!";
        let truncated = truncate_at_char_boundary(text, 5);
        assert!(truncated.starts_with("Hello"));
        assert!(truncated.contains("[Content truncated]"));
    }

    #[test]
    fn test_truncate_no_truncation_needed() {
        let text = "short";
        let result = truncate_at_char_boundary(text, 100);
        assert_eq!(result, "short");
    }

    #[test]
    fn test_truncate_multibyte_chars() {
        // Ensure we don't split in the middle of a multi-byte character
        let text = "Hello 🌍 World";
        let truncated = truncate_at_char_boundary(text, 7);
        // Should not panic and should produce valid UTF-8
        assert!(truncated.len() >= 6);
        assert!(truncated.contains("[Content truncated]"));
    }

    #[tokio::test]
    async fn test_fetch_tool_definition() {
        let tool = FetchTool::new();
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "fetch");
        assert!(def.description.contains("Fetch a URL"));
    }

    #[tokio::test]
    async fn test_fetch_tool_invalid_url() {
        let tool = FetchTool::new();
        let args = FetchToolArgs {
            url: "not-a-url".to_string(),
            max_length: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }
}
