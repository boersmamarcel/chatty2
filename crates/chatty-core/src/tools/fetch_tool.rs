#[cfg(test)]
use rig_agent::tool::tool_definition;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

use crate::tools::ToolError;

/// Default maximum response length, in bytes of UTF-8 text
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
    /// Maximum length of the returned content, in bytes of UTF-8 text
    /// (default: 50000)
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Byte offset into the *extracted* text to start the returned window at
    /// (default: 0). Continues a fetch that came back truncated without
    /// re-fetching from the top (AGE-507).
    #[serde(default)]
    pub start_index: Option<usize>,
    /// Override the User-Agent header for this request. Useful when a site's
    /// automated-traffic policy asks for a specific declared identity (e.g.
    /// SEC EDGAR — AGE-496) that the default Chatty user-agent doesn't satisfy.
    #[serde(default)]
    pub user_agent: Option<String>,
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
        let client = crate::services::http_client::no_redirect_client(REQUEST_TIMEOUT_SECS);
        Self {
            client,
            workspace_dir,
        }
    }

    /// Build a GET request, overriding the client's default User-Agent when
    /// the caller declared one (AGE-496).
    fn request(&self, url: &str, user_agent: Option<&str>) -> reqwest::RequestBuilder {
        let request = self.client.get(url);
        match user_agent {
            Some(ua) => request.header(reqwest::header::USER_AGENT, ua),
            None => request,
        }
    }
}

impl Tool for FetchTool {
    const NAME: &'static str = "fetch";
    type Error = ToolError;
    type Args = FetchToolArgs;
    type Output = FetchToolOutput;

    fn description(&self) -> String {
        "Fetch a URL and return its content. \
                         HTML pages are automatically converted to plain text for readability, \
                         with navigation chrome dropped and hyperlinks kept as `text (url)` so you can follow them. \
                         A URL with a #fragment starts the text at that section of the page. \
                         If the result says it was truncated, continue it with `start_index` \
                         rather than fetching the same URL again. \
                         Binary content (images, PDFs, zip files, etc.) is saved to the workspace directory. \
                         Only performs GET requests (read-only). \
                         Use this to look up documentation, read web pages, fetch API responses, or download files."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch. HTTPS is preferred for security."
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum length of returned content, in bytes of UTF-8 text. Defaults to 50000 — leave it unset unless you deliberately want a smaller window."
                },
                "start_index": {
                    "type": "integer",
                    "description": "Where to start the returned window in the extracted text, as a byte offset into it. Defaults to 0. When a response comes back truncated it names the start_index to continue from, so pass that instead of re-fetching the page."
                },
                "user_agent": {
                    "type": "string",
                    "description": "Override the User-Agent header for this request. Use this when a site rejects the default identity and states what it expects (e.g. \"declare an automated tool with contact info\")."
                }
            },
            "required": ["url"]
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
        let url = args.url.trim().to_string();
        let max_length = args.max_length.unwrap_or(DEFAULT_MAX_LENGTH);
        let start_index = args.start_index.unwrap_or(0);
        // The fragment is a client-side instruction — it never reaches the
        // server, and a redirect can drop it — so read it off what was asked
        // for, not off the URL we ended up at.
        let fragment = url
            .split_once('#')
            .map(|(_, fragment)| fragment.to_string())
            .filter(|fragment| !fragment.is_empty());

        // Validate URL scheme
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ToolError::OperationFailed(
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
            .request(&current_url, args.user_agent.as_deref())
            .send()
            .await
            .map_err(|e| ToolError::OperationFailed(format!("Request failed: {}", e)))?;

        for _ in 0..10 {
            if !response.status().is_redirection() {
                break;
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    ToolError::OperationFailed(
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
                    ToolError::OperationFailed(format!("Invalid base URL for redirect: {}", e))
                })?;
                base.join(&location)
                    .map_err(|e| {
                        ToolError::OperationFailed(format!("Invalid redirect URL: {}", e))
                    })?
                    .to_string()
            };

            // SSRF protection: validate redirect target
            validate_url_host(&next_url)?;

            info!(from = %current_url, to = %next_url, "Following redirect");
            current_url = next_url;

            response = self
                .request(&current_url, args.user_agent.as_deref())
                .send()
                .await
                .map_err(|e| ToolError::OperationFailed(format!("Redirect failed: {}", e)))?;
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
            let (body, truncated) = window(&body, start_index, max_length);
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
            ToolError::OperationFailed(format!("Failed to read response body: {}", e))
        })?;

        // Convert HTML to readable text if appropriate. Relative links resolve
        // against the URL the body actually came from, i.e. after redirects.
        let is_html = content_type.contains("text/html") || looks_like_html(&body);
        let content = if is_html {
            html_to_text(&body, Some(&current_url), fragment.as_deref())
        } else {
            body
        };

        // Return the requested window of it
        let extracted_len = content.len();
        let (content, truncated) = window(&content, start_index, max_length);
        if truncated {
            warn!(
                original_len = extracted_len,
                start_index = start_index,
                max_length = max_length,
                "Truncating response content"
            );
        }

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
    ) -> Result<FetchToolOutput, ToolError> {
        let workspace = self.workspace_dir.as_ref().ok_or_else(|| {
            ToolError::OperationFailed(
                "Cannot download binary files: no workspace directory configured. \
                 Set a workspace directory in Settings > Code Execution to enable file downloads."
                    .to_string(),
            )
        })?;

        // Read binary body
        let bytes = response.bytes().await.map_err(|e| {
            ToolError::OperationFailed(format!("Failed to read response body: {}", e))
        })?;

        if bytes.len() > MAX_BINARY_BYTES {
            return Err(ToolError::OperationFailed(format!(
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
            ToolError::OperationFailed(format!(
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
/// Delegates to the shared SSRF guard (`services::ssrf_guard`) so this tool and the
/// browser's internet-enabled navigation policy enforce the same denylist.
fn validate_url_host(url: &str) -> Result<(), ToolError> {
    crate::services::ssrf_guard::check_public_host(url)
        .map_err(|reason| ToolError::OperationFailed(format!("Access denied: {reason}")))
}

/// Simple heuristic to detect HTML content when content-type is missing or ambiguous
fn looks_like_html(body: &str) -> bool {
    let trimmed = body.trim_start();
    trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
}

/// The largest char boundary at or before `index`, so a window never splits a
/// multi-byte character. Both edges of a window use this one rule, which is
/// what makes consecutive windows meet exactly (see `window`).
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut end = index.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Take the `max_length` window of `s` that starts at `start_index`, returning
/// it with a flag saying whether anything was left over.
///
/// Truncation is not a dead end: the note names the `start_index` to continue
/// from, and because both the end of one window and the start of the next are
/// snapped with `floor_char_boundary`, `(0, n)` followed by `(n, n)` reproduce
/// `s` with no gap and no overlap (AGE-507).
fn window(s: &str, start_index: usize, max_length: usize) -> (String, bool) {
    let start = floor_char_boundary(s, start_index);
    let rest = &s[start..];
    if rest.len() <= max_length {
        return (rest.to_string(), false);
    }
    let end = floor_char_boundary(rest, max_length);
    // A window narrower than the character at its start floors to 0, which
    // would return nothing and name the same start_index again — a model
    // following the note would page forever. Emit one whole character instead.
    let end = if end == 0 {
        rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)
    } else {
        end
    };
    let mut result = rest[..end].to_string();
    result.push_str(&format!(
        "\n\n[Content truncated. Continue with start_index={}]",
        start + end
    ));
    (result, true)
}

/// Elements whose text is site furniture rather than page content. Skipped
/// whole, the way `<script>`/`<style>` are: on a Wikipedia article they are the
/// "Jump to content / Main menu / …" preamble that used to fill a small
/// `max_length` window before the article body was ever reached (AGE-507).
const CHROME_TAGS: [&str; 4] = ["nav", "header", "footer", "aside"];

/// The lowercase tag name of a raw tag body (`a href="/x"` → `a`,
/// `/div ` → `/div`), keeping the leading slash of a closing tag.
fn tag_name(tag: &str) -> String {
    tag.chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '/')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// The value of attribute `name` in a raw tag body, quoted or not.
///
/// Attribute names are matched whole and case-insensitively, so looking for
/// `id` doesn't match `data-id` and looking for `href` doesn't match `xhref`.
/// Entity references in the value are left alone — the whole extracted text is
/// decoded once at the end, and decoding here too would decode twice.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    // Every index below lands on a char boundary: the scan only ever stops on
    // an ASCII delimiter, and a UTF-8 continuation byte is never one.
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1; // the tag name itself
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let key = &tag[key_start..i];
        if key.is_empty() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue; // valueless attribute, e.g. `disabled`
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let value = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
            let quote = bytes[j];
            j += 1;
            let value_start = j;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            let value = &tag[value_start..j];
            j = (j + 1).min(bytes.len());
            value
        } else {
            let value_start = j;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            &tag[value_start..j]
        };
        i = j;
        if key.eq_ignore_ascii_case(name) {
            return Some(value.to_string());
        }
    }
    None
}

/// Turn an `href` into an absolute URL to print next to its link text.
///
/// Returns `None` for links that would only add noise: empty hrefs, in-page
/// `#fragment` links (the model can build those from the page URL itself), and
/// `javascript:` handlers.
fn resolve_href(base: Option<&reqwest::Url>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    // `get` rather than `href[..11]`: an href whose 11th byte falls inside a
    // multi-byte character (`αααααα.html`) would panic the whole fetch call.
    if href
        .get(..11)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("javascript:"))
    {
        return None;
    }
    match base {
        Some(base) => base.join(href).ok().map(String::from),
        None => Some(href.to_string()),
    }
}

/// Convert HTML to readable plain text.
///
/// Comprehensive HTML-to-text converter: strips tags, skips script/style and
/// navigation-chrome blocks, keeps hyperlinks as `text (absolute-url)` and
/// table cells separated by ` | `, inserts newlines for block-level elements,
/// normalises whitespace, and decodes entities. Contrast with
/// `search_web_tool::strip_html_tags`, which is a lightweight tag stripper for
/// short search-result snippets.
///
/// `base_url` is the URL the body came from; relative `href`s resolve against
/// it. `fragment` is the `#fragment` that was asked for: if an element carries
/// a matching `id`, the text starts there instead of at the top of the page.
fn html_to_text(html: &str, base_url: Option<&str>, fragment: Option<&str>) -> String {
    let base = base_url.and_then(|url| reqwest::Url::parse(url).ok());
    let mut result = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut chrome_depth: usize = 0;
    let mut last_was_whitespace = false;
    let mut tag = String::new();
    // Open `<a href>` elements: the URL to print, and how much text had been
    // written when the link opened — a link with no text gets no URL.
    let mut open_links: Vec<(String, usize)> = Vec::new();
    let mut fragment_start: Option<usize> = None;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag.clear();
            continue;
        }

        if in_tag {
            if ch != '>' {
                tag.push(ch);
                continue;
            }
            in_tag = false;

            let name = tag_name(&tag);
            let closing = name.starts_with('/');
            let element = name.trim_start_matches('/');

            // Blocks whose content is skipped entirely
            match element {
                "script" => in_script = !closing,
                "style" => in_style = !closing,
                _ if CHROME_TAGS.contains(&element) => {
                    chrome_depth = if closing {
                        chrome_depth.saturating_sub(1)
                    } else {
                        chrome_depth + 1
                    };
                }
                _ => {}
            }
            if in_script || in_style || chrome_depth > 0 {
                continue;
            }

            // Where the requested #fragment starts, if this is its element
            if let Some(fragment) = fragment
                && fragment_start.is_none()
                && !closing
                && attr_value(&tag, "id").as_deref() == Some(fragment)
            {
                fragment_start = Some(result.len());
            }

            // Hyperlinks: `text (absolute-url)`
            if element == "a" {
                if closing {
                    if let Some((href, opened_at)) = open_links.pop()
                        && !result[opened_at..].trim().is_empty()
                    {
                        result.truncate(result.trim_end_matches(' ').len());
                        result.push_str(&format!(" ({href})"));
                        last_was_whitespace = false;
                    }
                } else if let Some(href) =
                    attr_value(&tag, "href").and_then(|href| resolve_href(base.as_ref(), &href))
                {
                    open_links.push((href, result.len()));
                }
            }

            // Table cells: keep neighbouring cells apart whatever whitespace
            // the source HTML happens to have between them
            if !closing && matches!(element, "td" | "th") {
                let line = result.trim_end_matches(' ');
                if !line.is_empty() && !line.ends_with('\n') {
                    result.truncate(line.len());
                    result.push_str(" | ");
                    last_was_whitespace = true;
                }
            }

            // Add line breaks for block-level elements
            let is_block = matches!(
                element,
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
            continue;
        }

        // Skip content inside script, style and navigation-chrome blocks
        if in_script || in_style || chrome_depth > 0 {
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

    // Start at the requested #fragment when the page had it
    if let Some(offset) = fragment_start {
        result.drain(..offset);
    }

    // Decode HTML entities, once (`html_entities::decode_html_entities`)
    let result = crate::tools::html_entities::decode_html_entities(&result);

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

    /// Extract with no URL context — the shape most of these tests want.
    fn extract(html: &str) -> String {
        html_to_text(html, None, None)
    }

    #[test]
    fn test_html_to_text_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = extract(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_html_to_text_strips_script() {
        let html = "<p>Before</p><script>alert('xss')</script><p>After</p>";
        let text = extract(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_html_to_text_strips_style() {
        let html = "<p>Text</p><style>body { color: red; }</style><p>More</p>";
        let text = extract(html);
        assert!(text.contains("Text"));
        assert!(text.contains("More"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn test_html_to_text_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D &quot;E&quot;</p>";
        let text = extract(html);
        assert!(text.contains("A & B < C > D \"E\""));
    }

    // --- AGE-507 criterion 1: max_length is not required ---

    #[test]
    fn test_schema_does_not_require_max_length() {
        let schema = FetchTool::new(None).parameters();
        assert_eq!(
            schema["required"],
            serde_json::json!(["url"]),
            "only the URL is required; requiring max_length pushes the model to invent small values"
        );
        // …and it still has a default when omitted.
        let args: FetchToolArgs =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert_eq!(args.max_length, None);
        assert_eq!(args.max_length.unwrap_or(DEFAULT_MAX_LENGTH), 50_000);
    }

    // --- AGE-507 criterion 2: start_index continues a truncated fetch ---

    #[test]
    fn test_schema_offers_start_index() {
        let schema = FetchTool::new(None).parameters();
        assert!(schema["properties"]["start_index"].is_object());
        let args: FetchToolArgs = serde_json::from_value(
            serde_json::json!({ "url": "https://example.com", "start_index": 4000 }),
        )
        .unwrap();
        assert_eq!(args.start_index, Some(4000));
    }

    /// Two windows of the same size, the second continuing where the first
    /// stopped, must rebuild the text exactly: no gap, no overlap.
    ///
    /// This is the seam `call()` uses; an end-to-end `call()` test can't be
    /// written here because the SSRF guard blocks loopback by design, so a
    /// local test server is unreachable.
    #[test]
    fn test_window_chunks_join_without_gap_or_overlap() {
        let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let size = 1000;

        let (first, truncated) = window(&text, 0, size);
        assert!(truncated);
        let first_body = first.split("\n\n[Content truncated").next().unwrap();
        assert_eq!(first_body.len(), size);

        let (second, _) = window(&text, size, size);
        let second_body = second.split("\n\n[Content truncated").next().unwrap();

        assert_eq!(format!("{first_body}{second_body}"), text[..2 * size]);
        assert!(text.starts_with(&format!("{first_body}{second_body}")));
    }

    /// The whole text comes back when the windows are walked to the end.
    #[test]
    fn test_window_walks_the_whole_text() {
        let text: String = (0..200).map(|i| format!("row {i} • ")).collect();
        let size = 97; // deliberately lands mid-multibyte-character
        let mut rebuilt = String::new();
        let mut start = 0;
        loop {
            let (chunk, truncated) = window(&text, start, size);
            let body = chunk.split("\n\n[Content truncated").next().unwrap();
            rebuilt.push_str(body);
            if !truncated {
                break;
            }
            start += body.len();
        }
        assert_eq!(rebuilt, text);
    }

    /// The truncation note names the offset to continue from, so the model
    /// doesn't have to work it out (or give up, which is what it did).
    #[test]
    fn test_window_note_names_the_next_start_index() {
        let text = "abcdefghij";
        let (chunk, truncated) = window(text, 0, 4);
        assert!(truncated);
        assert_eq!(
            chunk,
            "abcd\n\n[Content truncated. Continue with start_index=4]"
        );
        let (rest, truncated) = window(text, 4, 4);
        assert!(truncated);
        assert!(rest.starts_with("efgh"));
    }

    /// A window narrower than the character it starts on must still make
    /// progress: returning nothing while naming the same `start_index` would
    /// send a model that follows the note round the same call forever.
    #[test]
    fn test_window_narrower_than_a_character_still_advances() {
        let text = "a🌍béc日本語d";
        for size in 1..=4 {
            let mut start = 0;
            let mut rebuilt = String::new();
            let mut steps = 0;
            loop {
                let (chunk, truncated) = window(text, start, size);
                let body = chunk.split("\n\n[Content truncated").next().unwrap();
                assert!(
                    !body.is_empty(),
                    "empty window at start={start} size={size} would stall paging"
                );
                rebuilt.push_str(body);
                if !truncated {
                    break;
                }
                start += body.len();
                steps += 1;
                assert!(steps < 100, "paging did not terminate at size={size}");
            }
            assert_eq!(rebuilt, text, "size={size} lost or duplicated text");
        }
    }

    #[test]
    fn test_window_past_the_end_is_empty() {
        let (chunk, truncated) = window("short", 100, 10);
        assert_eq!(chunk, "");
        assert!(!truncated);
    }

    // --- AGE-507 criterion 3: navigation chrome is skipped ---

    #[test]
    fn test_html_to_text_skips_navigation_chrome() {
        let html = "<body><nav><a href=\"/menu\">Jump to content</a> Main menu</nav>\
                    <header>Site header</header>\
                    <p>Article body</p>\
                    <aside>Related pages</aside>\
                    <footer>Privacy policy</footer></body>";
        let text = extract(html);
        assert!(text.contains("Article body"));
        for chrome in [
            "Jump to content",
            "Main menu",
            "Site header",
            "Related pages",
            "Privacy policy",
            "/menu",
        ] {
            assert!(!text.contains(chrome), "chrome leaked into text: {text:?}");
        }
    }

    #[test]
    fn test_html_to_text_resumes_after_nested_chrome() {
        let html = "<nav>Menu <nav>Submenu</nav> More menu</nav><p>Body</p>";
        let text = extract(html);
        assert_eq!(text, "Body");
    }

    // --- AGE-507 criterion 4: #fragment starts the text at that element ---

    #[test]
    fn test_html_to_text_starts_at_fragment() {
        let html = "<body><p>Lead paragraph</p>\
                    <h2 id=\"route-description\">Route description</h2>\
                    <p>The route begins…</p></body>";
        let text = html_to_text(
            html,
            Some("https://example.com/road"),
            Some("route-description"),
        );
        assert!(text.starts_with("Route description"), "got {text:?}");
        assert!(!text.contains("Lead paragraph"));
        assert!(text.contains("The route begins"));
    }

    #[test]
    fn test_html_to_text_fragment_not_found_keeps_whole_page() {
        let html = "<p>Lead paragraph</p><h2 id=\"other\">Other</h2>";
        let text = html_to_text(html, Some("https://example.com/road"), Some("missing"));
        assert!(text.starts_with("Lead paragraph"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_fragment_matches_whole_id_only() {
        let html = "<p>Lead</p><span data-id=\"sec\">decoy</span><span id=\"sec\">Target</span>";
        let text = html_to_text(html, None, Some("sec"));
        assert!(text.starts_with("Target"), "got {text:?}");
    }

    // --- AGE-507 criterion 5: links and table structure survive extraction ---

    #[test]
    fn test_html_to_text_emits_links_with_absolute_urls() {
        let html = "<p>See <a href=\"/wiki/Route_66\">Route 66</a> and \
                    <a href=\"https://example.org/spec\">the spec</a>.</p>";
        let text = html_to_text(html, Some("https://en.wikipedia.org/wiki/Roads"), None);
        assert!(
            text.contains("Route 66 (https://en.wikipedia.org/wiki/Route_66)"),
            "got {text:?}"
        );
        assert!(
            text.contains("the spec (https://example.org/spec)"),
            "got {text:?}"
        );
    }

    #[test]
    fn test_html_to_text_resolves_relative_links_against_the_page_path() {
        let html = "<a href=\"sibling.html\">sibling</a> <a href=\"../up.html\">up</a>";
        let text = html_to_text(html, Some("https://example.com/docs/guide/page.html"), None);
        assert!(
            text.contains("sibling (https://example.com/docs/guide/sibling.html)"),
            "got {text:?}"
        );
        assert!(
            text.contains("up (https://example.com/docs/up.html)"),
            "got {text:?}"
        );
    }

    #[test]
    fn test_html_to_text_link_urls_are_entity_decoded_once() {
        let html = "<a href=\"/s?q=a&amp;p=2\">results</a>";
        let text = html_to_text(html, Some("https://example.com/"), None);
        assert!(
            text.contains("results (https://example.com/s?q=a&p=2)"),
            "got {text:?}"
        );
    }

    #[test]
    fn test_html_to_text_skips_noise_links() {
        let html = "<a href=\"#cite_note_1\">[1]</a> <a href=\"javascript:void(0)\">Toggle</a> \
                    <a href=\"/real\">Real</a>";
        let text = html_to_text(html, Some("https://example.com/page"), None);
        assert!(!text.contains("#cite_note_1"), "got {text:?}");
        assert!(!text.contains("javascript:"), "got {text:?}");
        assert!(
            text.contains("Real (https://example.com/real)"),
            "got {text:?}"
        );
    }

    /// A non-ASCII href used to panic the whole `fetch` call: the
    /// `javascript:` check sliced 11 raw bytes out of the href, and the 11th
    /// byte of `αααααα.html` is inside a character.
    #[test]
    fn test_html_to_text_handles_non_ascii_hrefs() {
        for (html, expect) in [
            ("<a href=\"αααααα.html\">Greek</a>", "Greek ("),
            ("<a href=\"日本語ペ.html\">Japanese</a>", "Japanese ("),
            ("<a href=\"ü.html\">Short</a>", "Short ("),
        ] {
            let text = html_to_text(html, Some("https://example.com/docs/"), None);
            assert!(text.contains(expect), "got {text:?}");
        }
        // …and the same href straight through `resolve_href`.
        let base = reqwest::Url::parse("https://example.com/docs/").unwrap();
        assert!(resolve_href(Some(&base), "αααααα.html").is_some());
        assert!(resolve_href(Some(&base), "日本語ペ.html").is_some());
        assert!(resolve_href(None, "αααααα.html").is_some());
        // A genuine javascript: href is still dropped.
        assert_eq!(resolve_href(Some(&base), "JavaScript:void(0)"), None);
    }

    #[test]
    fn test_html_to_text_link_without_text_emits_no_url() {
        let html = "<p>Before<a href=\"/icon\"><img src=\"i.png\"></a>After</p>";
        let text = html_to_text(html, Some("https://example.com/"), None);
        assert!(!text.contains("/icon"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_separates_table_cells() {
        // No whitespace at all between the cells: the separation has to come
        // from the tags, not from incidental whitespace in the source.
        let html = "<table><tr><th>Year</th><th>Winner</th></tr>\
                    <tr><td>1994</td><td>Brazil</td></tr></table>";
        let text = extract(html);
        assert!(text.contains("Year | Winner"), "got {text:?}");
        assert!(text.contains("1994 | Brazil"), "got {text:?}");
    }

    #[test]
    fn test_html_to_text_table_rows_stay_on_their_own_lines() {
        let html = "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>";
        let text = extract(html);
        assert_eq!(text, "a | b\nc | d");
    }

    // --- AGE-507 criterion 6: one entity decoder, no double decoding ---

    #[test]
    fn test_html_to_text_does_not_double_decode_entities() {
        // A page showing the literal text `&lt;` writes it as `&amp;lt;`.
        let html = "<p>Write &amp;lt; for a less-than sign</p>";
        let text = extract(html);
        assert_eq!(text, "Write &lt; for a less-than sign");
    }

    #[test]
    fn test_html_to_text_decodes_numeric_entities() {
        let html = "<p>it&#39;s &#x2014; fine</p>";
        let text = extract(html);
        assert_eq!(text, "it's — fine");
    }

    // --- helpers ---

    #[test]
    fn test_attr_value_reads_quoted_and_unquoted_values() {
        assert_eq!(
            attr_value("a href=\"/x\" id='y'", "href").as_deref(),
            Some("/x")
        );
        assert_eq!(
            attr_value("a href=\"/x\" id='y'", "id").as_deref(),
            Some("y")
        );
        assert_eq!(
            attr_value("a href=/x class=b", "href").as_deref(),
            Some("/x")
        );
        assert_eq!(attr_value("a HREF=\"/x\"", "href").as_deref(), Some("/x"));
        assert_eq!(attr_value("span data-id=\"n\"", "id"), None);
        assert_eq!(
            attr_value("a download href=\"/x\"", "href").as_deref(),
            Some("/x")
        );
        assert_eq!(attr_value("br/", "href"), None);
        assert_eq!(
            attr_value("a title=\"héllo wörld\" href=\"/x\"", "href").as_deref(),
            Some("/x")
        );
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
        let (truncated, flagged) = window("Hello, World!", 0, 5);
        assert!(truncated.starts_with("Hello"));
        assert!(truncated.contains("[Content truncated"));
        assert!(flagged);
    }

    #[test]
    fn test_truncate_no_truncation_needed() {
        let (result, truncated) = window("short", 0, 100);
        assert_eq!(result, "short");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_multibyte_chars() {
        // Ensure we don't split in the middle of a multi-byte character
        let (truncated, _) = window("Hello 🌍 World", 0, 7);
        // Should not panic and should produce valid UTF-8
        assert!(truncated.starts_with("Hello "));
        assert!(truncated.contains("[Content truncated"));
    }

    /// A window that starts mid-character starts at the character instead —
    /// the same rounding the previous window's end used, which is what makes
    /// the two meet exactly.
    #[test]
    fn test_window_start_snaps_to_char_boundary() {
        let text = "a🌍b";
        let (first, _) = window(text, 0, 3);
        let first_body = first.split("\n\n[Content truncated").next().unwrap();
        assert_eq!(first_body, "a");
        let (second, _) = window(text, 3, 10);
        assert_eq!(format!("{first_body}{second}"), text);
    }

    #[tokio::test]
    async fn test_fetch_tool_definition() {
        let tool = FetchTool::new(None);
        let def = tool_definition(&tool);
        assert_eq!(def.name, "fetch");
        assert!(def.description.contains("Fetch a URL"));
    }

    /// AGE-496: SEC EDGAR (and similar automated-traffic policies) reject
    /// requests that don't declare a contact; the default UA must keep one.
    #[test]
    fn test_default_user_agent_declares_a_contact() {
        assert!(crate::services::http_client::USER_AGENT.contains('@'));
    }

    #[tokio::test]
    async fn test_fetch_request_overrides_user_agent_when_declared() {
        let tool = FetchTool::new(None);
        let built = tool
            .request(
                "https://example.com",
                Some("Custom/1.0 contact@example.com"),
            )
            .build()
            .unwrap();
        assert_eq!(
            built.headers().get(reqwest::header::USER_AGENT).unwrap(),
            "Custom/1.0 contact@example.com"
        );
    }

    #[tokio::test]
    async fn test_fetch_request_uses_default_user_agent_when_not_declared() {
        // The client's own default User-Agent (set once in `no_redirect_client`)
        // is applied by reqwest at send time, not baked into `build()`'s
        // `Request` — so "no override" means no per-request header at all.
        let tool = FetchTool::new(None);
        let built = tool.request("https://example.com", None).build().unwrap();
        assert!(built.headers().get(reqwest::header::USER_AGENT).is_none());
    }

    /// AGE-496: confirms the EDGAR-compliant default UA actually gets past
    /// SEC's automated-traffic block, against the live API.
    ///
    /// ```
    /// cargo test -p chatty-core --lib fetches_sec_edgar_with_default_user_agent -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "hits the live data.sec.gov API"]
    async fn fetches_sec_edgar_with_default_user_agent() {
        let tool = FetchTool::new(None);
        let args = FetchToolArgs {
            url: "https://data.sec.gov/submissions/CIK0001408198.json".to_string(),
            max_length: None,
            start_index: None,
            user_agent: None,
        };
        let result = tool
            .call(&mut ToolContext::new(), args)
            .await
            .expect("EDGAR should accept the default declared user-agent");
        assert_eq!(result.status, 200);
    }

    #[tokio::test]
    async fn test_fetch_tool_invalid_url() {
        let tool = FetchTool::new(None);
        let args = FetchToolArgs {
            url: "not-a-url".to_string(),
            max_length: None,
            start_index: None,
            user_agent: None,
        };
        let result = tool.call(&mut ToolContext::new(), args).await;
        assert!(result.is_err());
    }

    /// AGE-507 criterion 2, end to end: a truncated fetch continued with
    /// `start_index` rebuilds the page with no gap and no overlap, and the
    /// `#fragment` lands on the section.
    ///
    /// This is the one criterion the in-process tests can only cover at the
    /// `window()` seam — `chatty-core` has no mock-server dev-dependency, and
    /// a local test server is unreachable on purpose (the SSRF guard blocks
    /// loopback, which `test_fetch_tool_blocks_ssrf` asserts). So it is an
    /// ignored live test, like `fetches_sec_edgar_with_default_user_agent`.
    ///
    /// The three calls are three separate requests: if the article is edited
    /// between them the seam shifts and this fails. That is inherent to paging
    /// a live page statelessly, not a bug in `start_index`.
    ///
    /// ```
    /// cargo test -p chatty-core --lib continues_a_truncated_fetch_with_start_index -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "hits the live en.wikipedia.org site"]
    async fn continues_a_truncated_fetch_with_start_index() {
        const URL: &str = "https://en.wikipedia.org/wiki/U.S._Route_66#Route_description";
        let tool = FetchTool::new(None);
        let fetch = async |start_index: Option<usize>, max_length: Option<usize>| {
            tool.call(
                &mut ToolContext::new(),
                FetchToolArgs {
                    url: URL.to_string(),
                    max_length,
                    start_index,
                    user_agent: None,
                },
            )
            .await
            .expect("Wikipedia should answer")
        };
        let strip = |s: &str| {
            s.split("\n\n[Content truncated")
                .next()
                .unwrap()
                .to_string()
        };

        let whole = fetch(None, Some(6000)).await;
        let first = fetch(Some(0), Some(3000)).await;
        let second = fetch(Some(3000), Some(3000)).await;

        // Criterion 4, live: the #fragment starts the text at that section.
        assert!(
            strip(&whole.content).starts_with("Route description"),
            "got {:?}",
            &whole.content[..80.min(whole.content.len())]
        );
        assert!(whole.truncated);
        assert_eq!(
            format!("{}{}", strip(&first.content), strip(&second.content)),
            strip(&whole.content),
            "two 3000-byte windows must rebuild one 6000-byte window exactly"
        );
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
    // Low-level is_private_ip/is_blocked_hostname coverage lives in
    // services::ssrf_guard, which this tool's validate_url_host delegates to.

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
            start_index: None,
            user_agent: None,
        };
        let result = tool.call(&mut ToolContext::new(), args).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("SSRF"),
            "Error should mention SSRF: {}",
            err_msg
        );
    }
}
