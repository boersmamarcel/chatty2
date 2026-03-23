//! `browser_extract` tool — extract structured data from the current page.
//!
//! Read-only — does not require approval.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend::TabId;
use crate::page::LinkInfo;
use crate::session::BrowserSession;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BrowserExtractError {
    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),
    #[error("Unknown extract type: {0}")]
    UnknownType(String),
}

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct BrowserExtractArgs {
    /// What to extract: "text", "links", "tables", "screenshot".
    pub extract: String,
}

#[derive(Debug, Serialize)]
pub struct BrowserExtractOutput {
    /// The type of data extracted.
    pub extract_type: String,
    /// Extracted text content (for "text" type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Extracted links (for "links" type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<LinkInfo>>,
    /// Extracted tables as rows of cells (for "tables" type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tables: Option<Vec<Vec<Vec<String>>>>,
    /// Screenshot saved to path (for "screenshot" type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
}

impl std::fmt::Display for BrowserExtractOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.extract_type.as_str() {
            "text" => {
                if let Some(text) = &self.text {
                    write!(f, "{text}")
                } else {
                    write!(f, "(no text)")
                }
            }
            "links" => {
                if let Some(links) = &self.links {
                    for link in links {
                        writeln!(f, "- [{}]({})", link.text, link.href)?;
                    }
                    Ok(())
                } else {
                    write!(f, "(no links)")
                }
            }
            "tables" => {
                if let Some(tables) = &self.tables {
                    writeln!(f, "{} table(s) found", tables.len())?;
                    for (i, table) in tables.iter().enumerate() {
                        writeln!(f, "\n### Table {}", i + 1)?;
                        for row in table {
                            writeln!(f, "| {} |", row.join(" | "))?;
                        }
                    }
                    Ok(())
                } else {
                    write!(f, "(no tables)")
                }
            }
            "screenshot" => {
                if let Some(path) = &self.screenshot_path {
                    write!(f, "Screenshot saved to: {path}")
                } else {
                    write!(f, "Screenshot capture failed")
                }
            }
            _ => write!(
                f,
                "{}",
                serde_json::to_string_pretty(self).unwrap_or_default()
            ),
        }
    }
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Extract structured data (text, links, tables, screenshot) from the current page.
///
/// Read-only — does not require approval.
#[derive(Clone)]
pub struct BrowserExtractTool {
    session: Arc<BrowserSession>,
    active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
}

impl BrowserExtractTool {
    pub fn new(
        session: Arc<BrowserSession>,
        active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
    ) -> Self {
        Self {
            session,
            active_tab,
        }
    }
}

impl Tool for BrowserExtractTool {
    const NAME: &'static str = "browser_extract";

    type Error = BrowserExtractError;
    type Args = BrowserExtractArgs;
    type Output = BrowserExtractOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browser_extract".to_string(),
            description: "Extract structured data from the current browser page.\n\
                Types:\n\
                - text: Get all visible text content\n\
                - links: Get all links with text and URLs\n\
                - tables: Get HTML tables as structured data\n\
                - screenshot: Capture a PNG screenshot"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "extract": {
                        "type": "string",
                        "enum": ["text", "links", "tables", "screenshot"],
                        "description": "What to extract from the page"
                    }
                },
                "required": ["extract"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let tab_guard = self.active_tab.read().await;
        let tab = tab_guard
            .as_ref()
            .ok_or_else(|| BrowserExtractError::ExtractionFailed("No active tab".into()))?;

        match args.extract.as_str() {
            "text" => {
                let text = self
                    .session
                    .extract_text(tab)
                    .await
                    .map_err(|e| BrowserExtractError::ExtractionFailed(e.to_string()))?;
                Ok(BrowserExtractOutput {
                    extract_type: "text".into(),
                    text: Some(text),
                    links: None,
                    tables: None,
                    screenshot_path: None,
                })
            }
            "links" => {
                let links = self
                    .session
                    .extract_links(tab)
                    .await
                    .map_err(|e| BrowserExtractError::ExtractionFailed(e.to_string()))?;
                Ok(BrowserExtractOutput {
                    extract_type: "links".into(),
                    text: None,
                    links: Some(links),
                    tables: None,
                    screenshot_path: None,
                })
            }
            "tables" => {
                let tables = self
                    .session
                    .extract_tables(tab)
                    .await
                    .map_err(|e| BrowserExtractError::ExtractionFailed(e.to_string()))?;
                Ok(BrowserExtractOutput {
                    extract_type: "tables".into(),
                    text: None,
                    links: None,
                    tables: Some(tables),
                    screenshot_path: None,
                })
            }
            "screenshot" => {
                let png = self
                    .session
                    .backend()
                    .screenshot(tab)
                    .await
                    .map_err(|e| BrowserExtractError::ExtractionFailed(e.to_string()))?;

                // Save to cache directory
                let cache_dir = dirs::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("chatty")
                    .join("browser_screenshots");
                tokio::fs::create_dir_all(&cache_dir).await.map_err(|e| {
                    BrowserExtractError::ExtractionFailed(format!(
                        "Failed to create screenshot directory {}: {e}",
                        cache_dir.display()
                    ))
                })?;
                let filename = format!(
                    "screenshot_{}.png",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                );
                let path = cache_dir.join(&filename);
                tokio::fs::write(&path, &png)
                    .await
                    .map_err(|e| BrowserExtractError::ExtractionFailed(e.to_string()))?;

                Ok(BrowserExtractOutput {
                    extract_type: "screenshot".into(),
                    text: None,
                    links: None,
                    tables: None,
                    screenshot_path: Some(path.to_string_lossy().to_string()),
                })
            }
            other => Err(BrowserExtractError::UnknownType(other.to_string())),
        }
    }
}
