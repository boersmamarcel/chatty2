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
use tracing::info;

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

        // Ensure session exists
        self.get_or_create_session().await?;

        // Navigate and build snapshot
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| {
            BrowseToolError::BrowseError("Session unexpectedly missing".to_string())
        })?;

        let snapshot = session
            .navigate(&url)
            .await
            .map_err(|e| BrowseToolError::BrowseError(format!("Navigation failed: {}", e)))?;

        Ok(BrowseToolOutput::from_snapshot(&snapshot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_repr::{PageSnapshot, PageState};

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
        };

        let output = BrowseToolOutput::from_snapshot(&snapshot);
        assert!(output.content.len() < 2100); // 2000 + "..."
        assert!(output.content.ends_with("..."));
    }
}
