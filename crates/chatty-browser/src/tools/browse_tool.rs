//! `browse` tool — navigate to a URL and return a [`PageSnapshot`].
//!
//! This is the primary read-only browser tool. It does not require approval.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    login_profiles: Vec<LoginProfile>,
}

impl BrowseTool {
    pub fn new(session: Arc<BrowserSession>, login_profiles: Vec<LoginProfile>) -> Self {
        Self {
            session,
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
                // Fall back to HTTP fetch + HTML parsing
                tracing::info!(
                    url = %args.url,
                    backend_error = %backend_err,
                    "Browser backend unavailable, falling back to HTTP fetch"
                );
                let snapshot = crate::http_fallback::fetch_and_snapshot(&args.url)
                    .await
                    .map_err(|e| BrowseError::NavigationError(e.to_string()))?;
                Ok(BrowseOutput { snapshot })
            }
        }
    }
}

impl BrowseTool {
    /// Try to use the full browser backend (WebView) for navigation and snapshot.
    async fn try_browser_backend(&self, url: &str) -> Result<PageSnapshot, BrowseError> {
        let tab = self
            .session
            .backend()
            .new_tab()
            .await
            .map_err(|e| BrowseError::EngineError(e.to_string()))?;

        let snapshot = self
            .session
            .navigate_and_snapshot(&tab, url, &self.login_profiles)
            .await
            .map_err(|e| BrowseError::NavigationError(e.to_string()))?;

        // Close the tab after use (best effort)
        if let Err(e) = self.session.backend().close_tab(&tab).await {
            tracing::warn!(error = ?e, "Failed to close browse tab");
        }

        Ok(snapshot)
    }
}
