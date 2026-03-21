//! `browser_tabs` tool — manage browser tabs.
//!
//! Read-only operations — does not require approval.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend::TabId;
use crate::session::BrowserSession;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BrowserTabsError {
    #[error("Tab operation failed: {0}")]
    OperationFailed(String),
    #[error("Unknown action: {0}")]
    UnknownAction(String),
}

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct BrowserTabsArgs {
    /// Action: "new", "close", "switch", "list".
    pub action: String,
    /// Tab ID for close/switch actions.
    #[serde(default)]
    pub tab_id: Option<String>,
    /// URL to navigate to when opening a new tab.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrowserTabsOutput {
    pub success: bool,
    /// For "list": info about all tabs. For "new": the new tab's ID.
    pub result: String,
    /// For "new" and "switch": the active tab ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tab_id: Option<String>,
}

impl std::fmt::Display for BrowserTabsOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.result)
    }
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Manage browser tabs (new, close, switch, list).
#[derive(Clone)]
pub struct BrowserTabsTool {
    session: Arc<BrowserSession>,
    active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
}

impl BrowserTabsTool {
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

impl Tool for BrowserTabsTool {
    const NAME: &'static str = "browser_tabs";

    type Error = BrowserTabsError;
    type Args = BrowserTabsArgs;
    type Output = BrowserTabsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browser_tabs".to_string(),
            description: "Manage browser tabs.\n\
                Actions:\n\
                - new: Open a new tab (optionally navigate to a URL)\n\
                - close: Close a tab by ID\n\
                - switch: Switch to a tab by ID\n\
                - list: List all open tabs"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["new", "close", "switch", "list"],
                        "description": "Tab management action"
                    },
                    "tab_id": {
                        "type": "string",
                        "description": "Tab ID for close/switch actions"
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to when opening a new tab"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.action.as_str() {
            "new" => {
                let tab = self
                    .session
                    .backend()
                    .new_tab()
                    .await
                    .map_err(|e| BrowserTabsError::OperationFailed(e.to_string()))?;

                if let Some(url) = &args.url {
                    self.session
                        .backend()
                        .navigate(&tab, url)
                        .await
                        .map_err(|e| BrowserTabsError::OperationFailed(e.to_string()))?;
                }

                let tab_id = tab.0.clone();
                *self.active_tab.write().await = Some(tab);

                Ok(BrowserTabsOutput {
                    success: true,
                    result: format!("New tab opened: {tab_id}"),
                    active_tab_id: Some(tab_id),
                })
            }
            "close" => {
                let tab_id_str = args
                    .tab_id
                    .ok_or_else(|| BrowserTabsError::OperationFailed("tab_id required".into()))?;
                let tab = TabId(tab_id_str.clone());

                self.session
                    .backend()
                    .close_tab(&tab)
                    .await
                    .map_err(|e| BrowserTabsError::OperationFailed(e.to_string()))?;

                // If we closed the active tab, clear it
                let mut active = self.active_tab.write().await;
                if active.as_ref().map(|t| &t.0) == Some(&tab_id_str) {
                    *active = None;
                }

                Ok(BrowserTabsOutput {
                    success: true,
                    result: format!("Tab {tab_id_str} closed"),
                    active_tab_id: None,
                })
            }
            "switch" => {
                let tab_id_str = args
                    .tab_id
                    .ok_or_else(|| BrowserTabsError::OperationFailed("tab_id required".into()))?;
                let tab = TabId(tab_id_str.clone());
                *self.active_tab.write().await = Some(tab);

                Ok(BrowserTabsOutput {
                    success: true,
                    result: format!("Switched to tab {tab_id_str}"),
                    active_tab_id: Some(tab_id_str),
                })
            }
            "list" => {
                let tabs = self.session.backend().list_tabs();
                let active = self.active_tab.read().await;
                let active_id = active.as_ref().map(|t| t.0.clone());

                let mut lines = Vec::new();
                for tab in &tabs {
                    let marker = if Some(&tab.id.0) == active_id.as_ref() {
                        " (active)"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "- {}: {} [{}]{}",
                        tab.id, tab.title, tab.url, marker
                    ));
                }

                if lines.is_empty() {
                    lines.push("No tabs open".to_string());
                }

                Ok(BrowserTabsOutput {
                    success: true,
                    result: lines.join("\n"),
                    active_tab_id: active_id,
                })
            }
            other => Err(BrowserTabsError::UnknownAction(other.to_string())),
        }
    }
}
