//! `browse` tool — navigate to a URL and return a [`PageSnapshot`].
//!
//! This is the primary read-only browser tool. It does not require approval.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::backend::TabId;
use crate::credential::types::LoginProfile;
use crate::page::PageSnapshot;
use crate::session::BrowserSession;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BrowseError {
    #[error("Navigation failed: {0}")]
    NavigationError(String),
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Browser engine error: {0}")]
    EngineError(String),
}

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct BrowseArgs {
    /// The URL to navigate to.
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct BrowseOutput {
    /// Structured page snapshot (title, text, elements, forms, links).
    pub snapshot: PageSnapshot,
}

impl std::fmt::Display for BrowseOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.snapshot)
    }
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Navigate to a URL and return a structured page snapshot.
///
/// Read-only — does not require approval.
#[derive(Clone)]
pub struct BrowseTool {
    session: Arc<BrowserSession>,
    active_tab: Arc<RwLock<Option<TabId>>>,
    login_profiles: Vec<LoginProfile>,
}

impl BrowseTool {
    pub fn new(
        session: Arc<BrowserSession>,
        active_tab: Arc<RwLock<Option<TabId>>>,
        login_profiles: Vec<LoginProfile>,
    ) -> Self {
        Self {
            session,
            active_tab,
            login_profiles,
        }
    }
}

impl Tool for BrowseTool {
    const NAME: &'static str = "browse";

    type Error = BrowseError;
    type Args = BrowseArgs;
    type Output = BrowseOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browse".to_string(),
            description: "Navigate to a URL and return a structured page snapshot. \
                Returns the page title, text content (truncated), interactive elements \
                (with stable IDs like e1, e2), forms, and links. Use this to read \
                web pages. Falls back to HTTP fetch when the browser engine is \
                unavailable (interactive elements will be empty in fallback mode)."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to navigate to (must start with http:// or https://)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate URL
        let parsed = url::Url::parse(&args.url)
            .map_err(|e| BrowseError::InvalidUrl(format!("{}: {}", args.url, e)))?;

        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(BrowseError::InvalidUrl(format!(
                "Only http:// and https:// URLs are supported, got {scheme}://"
            )));
        }

        // Try the full browser backend first
        match self.try_browser_backend(&args.url).await {
            Ok(snapshot) => Ok(BrowseOutput { snapshot }),
            Err(backend_err) => {
                // Fall back to HTTP fetch + HTML parsing (with shared cookie jar
                // so cookies from browser_auth are sent with the request)
                tracing::warn!(
                    url = %args.url,
                    backend_error = %backend_err,
                    "Browser backend unavailable, falling back to HTTP fetch"
                );
                let jar = self.session.cookie_jar().clone();
                let snapshot =
                    crate::http_fallback::fetch_and_snapshot_with_cookies(&args.url, Some(jar))
                        .await
                        .map_err(|e| BrowseError::NavigationError(e.to_string()))?;
                Ok(BrowseOutput { snapshot })
            }
        }
    }
}

impl BrowseTool {
    /// Try to use the full browser backend (WebView) for navigation and snapshot.
    ///
    /// Reuses the shared `active_tab` when available so that authentication
    /// state (cookies, session) persists across tool calls.
    async fn try_browser_backend(&self, url: &str) -> Result<PageSnapshot, BrowseError> {
        // Reuse the active tab if one exists, otherwise create a new one
        let tab = {
            let mut tab_guard = self.active_tab.write().await;
            if let Some(existing) = tab_guard.as_ref() {
                existing.clone()
            } else {
                let new_tab = self
                    .session
                    .backend()
                    .new_tab()
                    .await
                    .map_err(|e| BrowseError::EngineError(e.to_string()))?;
                *tab_guard = Some(new_tab.clone());
                new_tab
            }
        };

        let mut snapshot = self
            .session
            .navigate_and_snapshot(&tab, url, &self.login_profiles)
            .await
            .map_err(|e| BrowseError::NavigationError(e.to_string()))?;

        // Capture screenshot (best effort — don't fail the browse if screenshot fails)
        match self.session.backend().screenshot(&tab).await {
            Ok(png_bytes) => match save_screenshot_to_cache(&png_bytes).await {
                Ok(path) => {
                    snapshot.screenshot_path = Some(path);
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "Failed to save screenshot to cache");
                }
            },
            Err(e) => {
                tracing::debug!(error = ?e, "Screenshot capture failed (non-fatal)");
            }
        }

        // Tab is kept alive for session persistence across calls

        Ok(snapshot)
    }
}

/// Save PNG bytes to the browser screenshots cache directory.
/// Returns the full file path as a string.
async fn save_screenshot_to_cache(png_bytes: &[u8]) -> anyhow::Result<String> {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("chatty")
        .join("browser_screenshots");
    tokio::fs::create_dir_all(&cache_dir).await?;

    let filename = format!(
        "browse_{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let path = cache_dir.join(&filename);
    tokio::fs::write(&path, png_bytes).await?;
    Ok(path.to_string_lossy().to_string())
}
