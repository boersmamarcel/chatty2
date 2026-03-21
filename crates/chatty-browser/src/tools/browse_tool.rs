//! The `browse` tool — navigate to a URL and return a structured page snapshot.
//!
//! This is the primary entry point for the LLM agent to access web content
//! through the Verso browser engine.

use crate::engine::BrowserEngine;
use crate::page_repr::PageSnapshot;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Maximum characters of text content returned in the tool output.
/// Shorter than the LLM snapshot rendering (4000) because the full snapshot
/// is also included in the output and contains the same text content.
const TOOL_OUTPUT_TEXT_MAX_CHARS: usize = 2000;

/// Arguments for the browse tool.
#[derive(Deserialize, Serialize)]
pub struct BrowseToolArgs {
    /// The URL to navigate to.
    pub url: String,
}

/// Output from the browse tool — a page snapshot rendered for the LLM.
#[derive(Debug, Serialize)]
pub struct BrowseToolOutput {
    /// Page title.
    pub title: String,
    /// Current URL (may differ from requested URL due to redirects).
    pub url: String,
    /// Readable text summary of the page content.
    pub content: String,
    /// Number of interactive elements found.
    pub interactive_element_count: usize,
    /// Number of forms found.
    pub form_count: usize,
    /// Number of links found.
    pub link_count: usize,
    /// Human-readable page representation for the LLM.
    pub page_snapshot: String,
    /// Local file path to a cached preview image (OG image), if available.
    /// The UI uses this to display a visual preview of the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    /// Local file path to a cached copy of the page's raw HTML.
    /// The UI uses this to render a visual website preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_cache_path: Option<String>,
}

impl BrowseToolOutput {
    /// Build output from a [`PageSnapshot`].
    fn from_snapshot(snapshot: &PageSnapshot) -> Self {
        Self {
            title: snapshot.title.clone(),
            url: snapshot.url.clone(),
            content: if snapshot.text_content.len() > TOOL_OUTPUT_TEXT_MAX_CHARS {
                format!(
                    "{}...",
                    &snapshot.text_content[..TOOL_OUTPUT_TEXT_MAX_CHARS]
                )
            } else {
                snapshot.text_content.clone()
            },
            interactive_element_count: snapshot.elements.len(),
            form_count: snapshot.forms.len(),
            link_count: snapshot.links.len(),
            page_snapshot: snapshot.to_llm_text(),
            screenshot_path: None, // Set by call() after OG image download
            html_cache_path: None, // Set by call() after HTML caching
        }
    }
}

/// Error type for the browse tool.
#[derive(Debug, thiserror::Error)]
pub enum BrowseToolError {
    #[error("Browse error: {0}")]
    BrowseError(String),
}

/// The `browse` tool: navigates to a URL using the Verso browser engine
/// and returns a structured page snapshot.
#[derive(Clone)]
pub struct BrowseTool {
    /// Shared browser engine instance.
    engine: Arc<BrowserEngine>,
    /// Per-tool session, lazily created and reused across calls.
    session: Arc<Mutex<Option<crate::session::BrowserSession>>>,
}

impl BrowseTool {
    /// Create a new browse tool backed by the given engine.
    pub fn new(engine: Arc<BrowserEngine>) -> Self {
        Self {
            engine,
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// Get or create the browser session for this tool instance.
    async fn get_or_create_session(&self) -> Result<(), BrowseToolError> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            // Ensure the engine is running
            if !self.engine.is_running().await {
                self.engine.start().await.map_err(|e| {
                    BrowseToolError::BrowseError(format!("Failed to start browser engine: {}", e))
                })?;
            }
            *guard = Some(self.engine.create_session());
        }
        Ok(())
    }
}

impl Tool for BrowseTool {
    const NAME: &'static str = "browse";
    type Error = BrowseToolError;
    type Args = BrowseToolArgs;
    type Output = BrowseToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browse".to_string(),
            description: "Navigate to a URL using a built-in browser engine and return the page \
                          content as a structured snapshot. The browser executes JavaScript and \
                          renders the page like a real browser, so it works with SPAs and \
                          dynamic content. Returns page text, interactive elements (buttons, \
                          inputs), forms, and links. Use this when you need to interact with \
                          web applications or access content that requires JavaScript rendering."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to navigate to. Must start with http:// or https://."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let url = args.url.trim().to_string();

        // Validate URL scheme
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(BrowseToolError::BrowseError(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        info!(url = %url, "Browse tool: navigating");

        // Try the full browser engine first; fall back to HTTP if unavailable
        let snapshot = match self.get_or_create_session().await {
            Ok(()) => {
                // Navigate and build snapshot via browser engine
                let mut guard = self.session.lock().await;
                let session = guard.as_mut().ok_or_else(|| {
                    BrowseToolError::BrowseError("Session unexpectedly missing".to_string())
                })?;

                session.navigate(&url).await.map_err(|e| {
                    BrowseToolError::BrowseError(format!("Navigation failed: {}", e))
                })?
            }
            Err(_engine_err) => {
                // Browser engine unavailable — fall back to plain HTTP fetch
                info!(
                    url = %url,
                    "Browser engine unavailable, using HTTP fallback"
                );

                crate::http_fallback::fetch_and_snapshot(&url)
                    .await
                    .map_err(|e| {
                        BrowseToolError::BrowseError(format!("HTTP fallback failed: {}", e))
                    })?
            }
        };

        let mut output = BrowseToolOutput::from_snapshot(&snapshot);

        // Try to download and cache the OG image for visual preview
        if let Some(ref og_url) = snapshot.og_image_url {
            match download_og_image(og_url, &snapshot.url).await {
                Ok(path) => {
                    debug!(path = %path, og_url = %og_url, "Cached OG image for preview");
                    output.screenshot_path = Some(path);
                }
                Err(e) => {
                    debug!(error = %e, og_url = %og_url, "Failed to download OG image");
                }
            }
        }

        // Cache the raw HTML for visual rendering in the UI
        if let Some(ref html) = snapshot.raw_html {
            match cache_html(html, &snapshot.url) {
                Ok(path) => {
                    debug!(path = %path, "Cached HTML for visual preview");
                    output.html_cache_path = Some(path);
                }
                Err(e) => {
                    debug!(error = %e, "Failed to cache HTML");
                }
            }
        }

        Ok(output)
    }
}

/// Maximum OG image file size (2 MB).
const MAX_OG_IMAGE_BYTES: usize = 2_000_000;

/// Maximum raw HTML cache size (2 MB).
const MAX_HTML_CACHE_BYTES: usize = 2_000_000;

/// Cache raw HTML to disk for visual rendering in the UI.
///
/// HTML is stored in the same browse cache directory as OG images,
/// using a hash of the page URL as filename with `.html` extension.
fn cache_html(html: &str, page_url: &str) -> Result<String, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    if html.len() > MAX_HTML_CACHE_BYTES {
        return Err(format!(
            "HTML too large to cache: {} bytes (max: {} bytes)",
            html.len(),
            MAX_HTML_CACHE_BYTES
        ));
    }

    let cache_dir = dirs::cache_dir()
        .ok_or("No cache directory")?
        .join("chatty")
        .join("browse_cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create browse cache dir: {}", e))?;

    let mut hasher = DefaultHasher::new();
    page_url.hash(&mut hasher);
    let hash = hasher.finish();

    let cache_path = cache_dir.join(format!("{:016x}.html", hash));

    std::fs::write(&cache_path, html).map_err(|e| format!("Failed to write HTML cache: {}", e))?;

    Ok(cache_path.to_string_lossy().to_string())
}

/// Download an OG image and cache it locally, returning the file path.
///
/// Images are cached in the platform-specific cache directory under
/// `chatty/browse_cache/`. The filename is a hash of the page URL to
/// ensure deterministic lookups from the trace view.
async fn download_og_image(og_url: &str, page_url: &str) -> Result<String, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Validate the OG image URL — must be external http/https
    if !og_url.starts_with("http://") && !og_url.starts_with("https://") {
        return Err("OG image URL must be http/https".to_string());
    }

    // Block requests to localhost and link-local addresses to prevent SSRF
    let url_lower = og_url.to_ascii_lowercase();
    if url_lower.contains("://localhost")
        || url_lower.contains("://127.")
        || url_lower.contains("://0.")
        || url_lower.contains("://[::1]")
        || url_lower.contains("://169.254.")
        || url_lower.contains("://10.")
        || url_lower.contains("://192.168.")
    {
        return Err("OG image URL points to a local/private address".to_string());
    }

    // Determine cache directory (use cache_dir, not config_dir)
    let cache_dir = dirs::cache_dir()
        .ok_or("No cache directory")?
        .join("chatty")
        .join("browse_cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create browse cache dir: {}", e))?;

    // Hash the page URL for the filename
    let mut hasher = DefaultHasher::new();
    page_url.hash(&mut hasher);
    let hash = hasher.finish();

    // Check if any cached file already exists for this hash
    let cache_prefix = format!("{:016x}.", hash);
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with(&cache_prefix)
            {
                return Ok(entry.path().to_string_lossy().to_string());
            }
        }
    }

    // Download the image
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Chatty/1.0 (Desktop AI Assistant)")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let response = client
        .get(og_url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch OG image: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("OG image HTTP {}", response.status()));
    }

    // Validate Content-Type to ensure it's actually an image (not SVG/HTML/etc.)
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let ext = if content_type.starts_with("image/png") {
        "png"
    } else if content_type.starts_with("image/gif") {
        "gif"
    } else if content_type.starts_with("image/webp") {
        "webp"
    } else if content_type.starts_with("image/jpeg") || content_type.starts_with("image/jpg") {
        "jpg"
    } else {
        return Err(format!(
            "OG image has unsupported Content-Type: {}",
            content_type
        ));
    };

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read OG image body: {}", e))?;

    if bytes.len() > MAX_OG_IMAGE_BYTES {
        return Err(format!("OG image too large: {} bytes", bytes.len()));
    }

    if bytes.is_empty() {
        return Err("OG image is empty".to_string());
    }

    let cache_path = cache_dir.join(format!("{:016x}.{}", hash, ext));

    std::fs::write(&cache_path, &bytes)
        .map_err(|e| format!("Failed to write OG image to cache: {}", e))?;

    info!(path = %cache_path.display(), size = bytes.len(), "Downloaded OG image for preview");

    Ok(cache_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrowserEngineConfig;
    use crate::page_repr::{PageSnapshot, PageState};
    use rig::tool::Tool;

    #[test]
    fn test_browse_output_from_snapshot() {
        let snapshot = PageSnapshot {
            url: "https://example.com".to_string(),
            title: "Example Domain".to_string(),
            text_content: "This domain is for use in examples.".to_string(),
            elements: vec![],
            forms: vec![],
            links: vec![crate::page_repr::LinkInfo {
                text: "More info".to_string(),
                href: "https://www.iana.org/domains/example".to_string(),
            }],
            state: PageState::Complete,
            og_image_url: None,
            description: None,
            raw_html: None,
        };

        let output = BrowseToolOutput::from_snapshot(&snapshot);
        assert_eq!(output.title, "Example Domain");
        assert_eq!(output.link_count, 1);
        assert_eq!(output.form_count, 0);
        assert!(output.page_snapshot.contains("Example Domain"));
    }

    #[test]
    fn test_browse_output_truncates_long_content() {
        let snapshot = PageSnapshot {
            url: "https://example.com".to_string(),
            title: "Long".to_string(),
            text_content: "x".repeat(5000),
            elements: vec![],
            forms: vec![],
            links: vec![],
            state: PageState::Complete,
            og_image_url: None,
            description: None,
            raw_html: None,
        };

        let output = BrowseToolOutput::from_snapshot(&snapshot);
        assert!(output.content.len() < 2100); // 2000 + "..."
        assert!(output.content.ends_with("..."));
    }

    /// End-to-end test: BrowseTool → BrowserEngine (mock) → BrowserSession (mock) → PageSnapshot.
    /// This exercises the full tool pipeline without needing a real browser.
    #[tokio::test]
    async fn test_browse_tool_mock_end_to_end() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(crate::engine::BrowserEngine::new(config));
        let tool = BrowseTool::new(engine);

        let output = tool
            .call(BrowseToolArgs {
                url: "https://example.com".to_string(),
            })
            .await
            .expect("Mock browse should succeed");

        assert!(output.title.contains("example.com"));
        assert_eq!(output.url, "https://example.com");
        assert!(!output.content.is_empty());
        assert_eq!(output.interactive_element_count, 3); // search input, search button, sign in link
        assert_eq!(output.form_count, 1); // search form
        assert_eq!(output.link_count, 3); // home, about, contact
        assert!(output.page_snapshot.contains("example.com"));
    }

    #[tokio::test]
    async fn test_browse_tool_mock_rejects_bad_url() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(crate::engine::BrowserEngine::new(config));
        let tool = BrowseTool::new(engine);

        let result = tool
            .call(BrowseToolArgs {
                url: "ftp://example.com".to_string(),
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_browse_tool_mock_definition() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(crate::engine::BrowserEngine::new(config));
        let tool = BrowseTool::new(engine);

        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "browse");
        assert!(def.description.contains("Navigate"));
    }

    #[test]
    fn test_cache_html_writes_file() {
        let html = "<html><head><title>Test</title></head><body><h1>Hello</h1></body></html>";
        let url = "https://test-cache-html.example.com";
        let result = super::cache_html(html, url);
        assert!(result.is_ok(), "cache_html should succeed");
        let path = result.unwrap();
        assert!(
            path.ends_with(".html"),
            "Cached file should have .html extension"
        );
        assert!(
            std::path::Path::new(&path).exists(),
            "Cached file should exist on disk"
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, html, "Cached contents should match input HTML");
        // Clean up
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_cache_html_rejects_oversized() {
        let html = "x".repeat(super::MAX_HTML_CACHE_BYTES + 1);
        let result = super::cache_html(&html, "https://example.com/big");
        assert!(result.is_err(), "Should reject HTML exceeding max size");
        assert!(result.unwrap_err().contains("too large"));
    }
}
