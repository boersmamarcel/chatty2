use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;
use tracing::{info, warn};

/// Default maximum response length in characters
const DEFAULT_MAX_LENGTH: usize = 50_000;

/// Maximum binary response size in bytes (10 MB)
const MAX_BINARY_BYTES: usize = 10 * 1024 * 1024;

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
    /// The readable text content of the response (empty for binary responses that were saved to disk)
    pub content: String,
    /// The content type of the response
    pub content_type: String,
    /// Whether the content was truncated due to max_length
    pub truncated: bool,
    /// Path to the saved file (only present for binary content like images, PDFs, zips)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_to: Option<String>,
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
/// Binary content (images, PDFs, zip files) is saved to the workspace directory.
/// Enforces timeouts, size limits, and HTTPS preference for safety.
#[derive(Clone)]
pub struct FetchTool {
    /// Reusable HTTP client with connection pooling, timeout, and SSRF-safe redirect policy.
    client: reqwest::Client,
    /// Optional workspace directory for saving downloaded binary files.
    /// When None, binary content returns an error asking the user to configure a workspace.
    workspace_dir: Option<PathBuf>,
}

impl FetchTool {
    pub fn new(workspace_dir: Option<PathBuf>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build HTTP client");
        Self {
            client,
            workspace_dir,
        }
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
            description: "Fetch a URL and return its content. \
                         HTML pages are automatically converted to plain text for readability. \
                         Binary content (images, PDFs, zip files, etc.) is saved to the workspace directory. \
                         Only performs GET requests (read-only). \
                         Use this to look up documentation, read web pages, fetch API responses, or download files."
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

        // Validate URL scheme
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(FetchToolError::FetchError(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        // SSRF protection: block requests to private/internal networks
        validate_url_host(&url)?;

        info!(url = %url, max_length = max_length, "Fetching URL");

        // Perform GET request, following redirects manually (max 10 hops)
        // to validate each redirect target against the private-host denylist.
        let mut current_url = url.clone();
        let mut response = self
            .client
            .get(&current_url)
            .send()
            .await
            .map_err(|e| FetchToolError::FetchError(format!("Request failed: {}", e)))?;

        for _ in 0..10 {
            if !response.status().is_redirection() {
                break;
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    FetchToolError::FetchError(
                        "Redirect response missing Location header".to_string(),
                    )
                })?
                .to_string();

            // Resolve relative redirects against the current URL
            let next_url = if location.starts_with("http://") || location.starts_with("https://") {
                location
            } else {
                // Relative URL — resolve against current
                let base = reqwest::Url::parse(&current_url).map_err(|e| {
                    FetchToolError::FetchError(format!("Invalid base URL for redirect: {}", e))
                })?;
                base.join(&location)
                    .map_err(|e| {
                        FetchToolError::FetchError(format!("Invalid redirect URL: {}", e))
                    })?
                    .to_string()
            };

            // SSRF protection: validate redirect target
            validate_url_host(&next_url)?;

            info!(from = %current_url, to = %next_url, "Following redirect");
            current_url = next_url;

            response = self
                .client
                .get(&current_url)
                .send()
                .await
                .map_err(|e| FetchToolError::FetchError(format!("Redirect failed: {}", e)))?;
        }

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
                saved_to: None,
            });
        }

        // Determine if this is binary content that should be saved to disk
        if is_binary_content_type(&content_type) {
            return self
                .handle_binary_response(response, &url, status, &content_type)
                .await;
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
            saved_to: None,
        })
    }
}

impl FetchTool {
    /// Handle binary responses by saving them to the workspace directory.
    async fn handle_binary_response(
        &self,
        response: reqwest::Response,
        url: &str,
        status: u16,
        content_type: &str,
    ) -> Result<FetchToolOutput, FetchToolError> {
        let workspace = self.workspace_dir.as_ref().ok_or_else(|| {
            FetchToolError::FetchError(
                "Cannot download binary files: no workspace directory configured. \
                 Set a workspace directory in Settings > Code Execution to enable file downloads."
                    .to_string(),
            )
        })?;

        // Read binary body
        let bytes = response.bytes().await.map_err(|e| {
            FetchToolError::FetchError(format!("Failed to read response body: {}", e))
        })?;

        if bytes.len() > MAX_BINARY_BYTES {
            return Err(FetchToolError::FetchError(format!(
                "Response too large: {} bytes (max {} bytes / {} MB)",
                bytes.len(),
                MAX_BINARY_BYTES,
                MAX_BINARY_BYTES / 1024 / 1024,
            )));
        }

        // Extract filename from URL or Content-Disposition header
        let filename = extract_filename(url, content_type);
        let save_path = workspace.join(&filename);

        // Ensure we don't overwrite existing files — add a numeric suffix if needed
        let save_path = unique_path(save_path);

        info!(
            path = %save_path.display(),
            size = bytes.len(),
            "Saving binary content to workspace"
        );

        tokio::fs::write(&save_path, &bytes).await.map_err(|e| {
            FetchToolError::FetchError(format!(
                "Failed to save file to {}: {}",
                save_path.display(),
                e
            ))
        })?;

        Ok(FetchToolOutput {
            status,
            content: format!(
                "Downloaded {} ({} bytes) and saved to: {}",
                content_type,
                bytes.len(),
                save_path.display()
            ),
            content_type: content_type.to_string(),
            truncated: false,
            saved_to: Some(save_path.to_string_lossy().to_string()),
        })
    }
}

/// Check if a content type indicates binary content that should be saved to disk.
fn is_binary_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.contains("application/pdf")
        || ct.contains("application/zip")
        || ct.contains("application/gzip")
        || ct.contains("application/x-tar")
        || ct.contains("application/x-gzip")
        || ct.contains("application/x-bzip2")
        || ct.contains("application/x-7z")
        || ct.contains("application/x-rar")
        || ct.contains("application/octet-stream")
        || ct.contains("application/vnd.openxmlformats") // docx, xlsx, pptx
        || ct.contains("application/msword")
        || ct.contains("application/vnd.ms-")
        || ct.contains("application/wasm")
}

/// Extract a reasonable filename from a URL and content type.
fn extract_filename(url: &str, content_type: &str) -> String {
    // Try to get filename from the URL path
    if let Some(path_segment) = url.split('?').next().and_then(|u| u.rsplit('/').next()) {
        let decoded = path_segment.to_string();
        if !decoded.is_empty() && decoded.contains('.') && decoded.len() <= 255 {
            // Sanitize: only keep alphanumeric, dots, hyphens, underscores
            let sanitized: String = decoded
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if !sanitized.is_empty() && sanitized != "." && sanitized != ".." {
                return sanitized;
            }
        }
    }

    // Fallback: generate name from content type
    let extension = match content_type.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "audio/mpeg" => "mp3",
        "video/mp4" => "mp4",
        _ => "bin",
    };
    format!("download.{}", extension)
}

/// Generate a unique file path by appending a numeric suffix if the file already exists.
fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let parent = path.parent().unwrap_or(std::path::Path::new("."));

    for i in 1..1000 {
        let candidate = parent.join(format!("{}-{}.{}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
    }

    // Extremely unlikely fallback
    parent.join(format!("{}-{}.{}", stem, uuid::Uuid::new_v4(), ext))
}

/// Validate that a URL does not target private, internal, or reserved network hosts.
///
/// Blocks loopback (127.x.x.x, ::1), RFC-1918 private ranges (10.x, 172.16-31.x, 192.168.x),
/// link-local (169.254.x.x, fe80::), cloud metadata endpoints (169.254.169.254), and other
/// reserved addresses to prevent SSRF attacks.
fn validate_url_host(url: &str) -> Result<(), FetchToolError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| FetchToolError::FetchError(format!("Invalid URL: {}", e)))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| FetchToolError::FetchError("URL has no host".to_string()))?;

    // Check hostname-based blocklist first (catches localhost even without DNS)
    if is_blocked_hostname(host) {
        return Err(FetchToolError::FetchError(format!(
            "Access denied: requests to '{}' are blocked for security (SSRF protection)",
            host
        )));
    }

    // Try to parse as IP address directly
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_private_ip(&ip)
    {
        return Err(FetchToolError::FetchError(format!(
            "Access denied: requests to private/internal IP '{}' are blocked for security (SSRF protection)",
            ip
        )));
    }

    // For hostnames, resolve to IP and check the resolved address.
    // This catches DNS rebinding / split-horizon attacks where a public hostname
    // resolves to a private IP.
    if host.parse::<IpAddr>().is_err() {
        // Use std::net for synchronous resolution (sufficient for validation)
        if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, 80)) {
            for addr in addrs {
                if is_private_ip(&addr.ip()) {
                    warn!(
                        host = %host,
                        resolved_ip = %addr.ip(),
                        "Blocked DNS-resolved private IP"
                    );
                    return Err(FetchToolError::FetchError(format!(
                        "Access denied: '{}' resolves to private/internal IP {} (SSRF protection)",
                        host,
                        addr.ip()
                    )));
                }
            }
        }
    }

    Ok(())
}

/// Check if a hostname string is a known-blocked name (case-insensitive).
fn is_blocked_hostname(host: &str) -> bool {
    let h = host.to_lowercase();
    h == "localhost"
        || h == "metadata.google.internal"  // GCP metadata
        || h.ends_with(".internal")
        || h.ends_with(".local")
}

/// Check if an IP address belongs to a private, loopback, link-local, or otherwise
/// reserved network range that should not be accessible from the fetch tool.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8 — loopback
            octets[0] == 127
            // 10.0.0.0/8 — RFC-1918 private
            || octets[0] == 10
            // 172.16.0.0/12 — RFC-1918 private
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16 — RFC-1918 private
            || (octets[0] == 192 && octets[1] == 168)
            // 169.254.0.0/16 — link-local (includes AWS/GCP/Azure metadata at 169.254.169.254)
            || (octets[0] == 169 && octets[1] == 254)
            // 0.0.0.0/8 — "this" network
            || octets[0] == 0
            // 100.64.0.0/10 — shared address space (CGN, often internal)
            || (octets[0] == 100 && (64..=127).contains(&octets[1]))
            // 198.18.0.0/15 — benchmarking
            || (octets[0] == 198 && (18..=19).contains(&octets[1]))
            // 224.0.0.0/4 — multicast
            || octets[0] >= 224
        }
        IpAddr::V6(v6) => {
            // ::1 — loopback
            v6.is_loopback()
            // fe80::/10 — link-local
            || (v6.segments()[0] & 0xffc0) == 0xfe80
            // fc00::/7 — unique local (ULA, RFC-4193)
            || (v6.segments()[0] & 0xfe00) == 0xfc00
            // :: — unspecified
            || v6.is_unspecified()
            // ::ffff:x.x.x.x — IPv4-mapped, check the embedded v4 address
            || v6.to_ipv4_mapped().map(|v4| is_private_ip(&IpAddr::V4(v4))).unwrap_or(false)
        }
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
        let tool = FetchTool::new(None);
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "fetch");
        assert!(def.description.contains("Fetch a URL"));
    }

    #[tokio::test]
    async fn test_fetch_tool_invalid_url() {
        let tool = FetchTool::new(None);
        let args = FetchToolArgs {
            url: "not-a-url".to_string(),
            max_length: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_is_binary_content_type() {
        assert!(is_binary_content_type("image/png"));
        assert!(is_binary_content_type("image/jpeg"));
        assert!(is_binary_content_type("application/pdf"));
        assert!(is_binary_content_type("application/zip"));
        assert!(is_binary_content_type("application/octet-stream"));
        assert!(is_binary_content_type(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        ));
        assert!(!is_binary_content_type("text/html"));
        assert!(!is_binary_content_type("text/plain"));
        assert!(!is_binary_content_type("application/json"));
    }

    #[test]
    fn test_extract_filename_from_url() {
        assert_eq!(
            extract_filename("https://example.com/photo.png", "image/png"),
            "photo.png"
        );
        assert_eq!(
            extract_filename("https://example.com/docs/report.pdf?v=2", "application/pdf"),
            "report.pdf"
        );
        assert_eq!(
            extract_filename("https://example.com/", "image/png"),
            "download.png"
        );
        assert_eq!(
            extract_filename("https://example.com/archive", "application/zip"),
            "download.zip"
        );
    }

    #[test]
    fn test_unique_path_no_conflict() {
        let path = PathBuf::from("/tmp/nonexistent_test_file_12345.txt");
        assert_eq!(unique_path(path.clone()), path);
    }

    // --- SSRF protection tests ---

    #[test]
    fn test_is_private_ip_loopback() {
        assert!(is_private_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"127.0.0.2".parse().unwrap()));
        assert!(is_private_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn test_is_private_ip_rfc1918() {
        assert!(is_private_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"10.255.255.255".parse().unwrap()));
        assert!(is_private_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_private_ip(&"172.31.255.255".parse().unwrap()));
        assert!(is_private_ip(&"192.168.0.1".parse().unwrap()));
        assert!(is_private_ip(&"192.168.255.255".parse().unwrap()));
    }

    #[test]
    fn test_is_private_ip_link_local_and_metadata() {
        // AWS/GCP/Azure metadata endpoint
        assert!(is_private_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_private_ip(&"169.254.0.1".parse().unwrap()));
    }

    #[test]
    fn test_is_private_ip_public() {
        assert!(!is_private_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip(&"93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn test_is_private_ip_other_reserved() {
        assert!(is_private_ip(&"0.0.0.0".parse().unwrap()));
        assert!(is_private_ip(&"100.64.0.1".parse().unwrap())); // CGN
        assert!(is_private_ip(&"224.0.0.1".parse().unwrap())); // multicast
        assert!(is_private_ip(&"255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn test_is_private_ip_v6() {
        // ULA
        assert!(is_private_ip(&"fd00::1".parse().unwrap()));
        // Link-local
        assert!(is_private_ip(&"fe80::1".parse().unwrap()));
        // Unspecified
        assert!(is_private_ip(&"::".parse().unwrap()));
    }

    #[test]
    fn test_is_private_ip_v4_mapped_v6() {
        // ::ffff:127.0.0.1 should be blocked
        assert!(is_private_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        // ::ffff:169.254.169.254 (metadata via v6)
        assert!(is_private_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        // ::ffff:8.8.8.8 should be allowed
        assert!(!is_private_ip(&"::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn test_is_blocked_hostname() {
        assert!(is_blocked_hostname("localhost"));
        assert!(is_blocked_hostname("LOCALHOST"));
        assert!(is_blocked_hostname("metadata.google.internal"));
        assert!(is_blocked_hostname("foo.internal"));
        assert!(is_blocked_hostname("printer.local"));
        assert!(!is_blocked_hostname("example.com"));
        assert!(!is_blocked_hostname("my-internal-api.com")); // "internal" in domain name is fine
    }

    #[test]
    fn test_validate_url_host_blocks_private() {
        assert!(validate_url_host("http://127.0.0.1/secret").is_err());
        assert!(validate_url_host("http://localhost:8080/admin").is_err());
        assert!(validate_url_host("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_url_host("http://10.0.0.1/internal").is_err());
        assert!(validate_url_host("http://192.168.1.1/router").is_err());
        assert!(validate_url_host("http://172.16.0.5/service").is_err());
        assert!(validate_url_host("http://[::1]/secret").is_err());
        assert!(validate_url_host("http://metadata.google.internal/computeMetadata/v1/").is_err());
    }

    #[test]
    fn test_validate_url_host_allows_public() {
        assert!(validate_url_host("https://example.com").is_ok());
        assert!(validate_url_host("https://docs.rs/rig-core/latest").is_ok());
    }

    #[tokio::test]
    async fn test_fetch_tool_blocks_ssrf() {
        let tool = FetchTool::new(None);
        let args = FetchToolArgs {
            url: "http://169.254.169.254/latest/meta-data/".to_string(),
            max_length: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("SSRF"),
            "Error should mention SSRF: {}",
            err_msg
        );
    }
}
