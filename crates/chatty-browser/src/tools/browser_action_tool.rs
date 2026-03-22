//! `browser_action` tool — interact with page elements.
//!
//! Supports: click, fill, select, scroll, wait.
//! Requires approval (configurable in BrowserSettings).

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend::TabId;
use crate::credential::types::LoginProfile;
use crate::page::PageSnapshot;
use crate::session::BrowserSession;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BrowserActionError {
    #[error("Action failed: {0}")]
    ActionFailed(String),
    #[error("Unknown action: {0}")]
    UnknownAction(String),
    #[error("Missing required parameter: {0}")]
    MissingParam(String),
}

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct BrowserActionArgs {
    /// The action to perform: "click", "fill", "select", "scroll", "wait".
    pub action: String,
    /// Element ID (e.g., "e1") for click, fill, select actions.
    #[serde(default)]
    pub element_id: Option<String>,
    /// Value for fill or select actions.
    #[serde(default)]
    pub value: Option<String>,
    /// Scroll distance in pixels (positive = down) for scroll action.
    #[serde(default)]
    pub scroll_pixels: Option<i32>,
    /// CSS selector to wait for (for wait action).
    #[serde(default)]
    pub selector: Option<String>,
    /// Timeout in milliseconds (for wait action, default 5000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct BrowserActionOutput {
    pub success: bool,
    pub result: String,
    /// Updated page snapshot after the action (for click, fill, select, scroll).
    /// Gives the LLM immediate visibility into the new page state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<PageSnapshot>,
}

impl std::fmt::Display for BrowserActionOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.result)?;
        if let Some(snapshot) = &self.snapshot {
            writeln!(f)?;
            writeln!(f, "--- Updated page state ---")?;
            write!(f, "{snapshot}")?;
        }
        Ok(())
    }
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Perform an action on a browser page (click, fill, select, scroll, wait).
///
/// Requires approval by default (configurable in browser settings).
#[derive(Clone)]
pub struct BrowserActionTool {
    session: Arc<BrowserSession>,
    active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
    login_profiles: Vec<LoginProfile>,
}

impl BrowserActionTool {
    pub fn new(
        session: Arc<BrowserSession>,
        active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
        login_profiles: Vec<LoginProfile>,
    ) -> Self {
        Self {
            session,
            active_tab,
            login_profiles,
        }
    }
}

impl Tool for BrowserActionTool {
    const NAME: &'static str = "browser_action";

    type Error = BrowserActionError;
    type Args = BrowserActionArgs;
    type Output = BrowserActionOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browser_action".to_string(),
            description: "Perform an action on the current browser page. Actions:\n\
                - click: Click an element by its ID (e.g., e1, e2)\n\
                - fill: Fill a form field with a value\n\
                - select: Select an option in a dropdown\n\
                - scroll: Scroll the page (positive = down, negative = up)\n\
                - wait: Wait for a CSS selector to appear\n\
                \n\
                Use the element IDs from the browse tool's output.\n\
                After click, fill, select, and scroll the updated page snapshot is returned \
                automatically so you can see the new state without calling browse again."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["click", "fill", "select", "scroll", "wait"],
                        "description": "The action to perform"
                    },
                    "element_id": {
                        "type": "string",
                        "description": "Element ID (e.g., 'e1') for click, fill, select"
                    },
                    "value": {
                        "type": "string",
                        "description": "Value for fill or select actions"
                    },
                    "scroll_pixels": {
                        "type": "integer",
                        "description": "Pixels to scroll (positive=down) for scroll action"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for wait action"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in ms for wait action (default 5000)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let tab_guard = self.active_tab.read().await;
        let tab = tab_guard
            .as_ref()
            .ok_or_else(|| BrowserActionError::ActionFailed("No active tab".into()))?;

        let action_name = args.action.clone();
        let result = match args.action.as_str() {
            "click" => {
                let id = args
                    .element_id
                    .ok_or_else(|| BrowserActionError::MissingParam("element_id".into()))?;
                self.session
                    .click_element(tab, &id)
                    .await
                    .map_err(|e| BrowserActionError::ActionFailed(e.to_string()))?
            }
            "fill" => {
                let id = args
                    .element_id
                    .ok_or_else(|| BrowserActionError::MissingParam("element_id".into()))?;
                let value = args
                    .value
                    .ok_or_else(|| BrowserActionError::MissingParam("value".into()))?;
                self.session
                    .fill_element(tab, &id, &value)
                    .await
                    .map_err(|e| BrowserActionError::ActionFailed(e.to_string()))?
            }
            "select" => {
                let id = args
                    .element_id
                    .ok_or_else(|| BrowserActionError::MissingParam("element_id".into()))?;
                let value = args
                    .value
                    .ok_or_else(|| BrowserActionError::MissingParam("value".into()))?;
                self.session
                    .select_option(tab, &id, &value)
                    .await
                    .map_err(|e| BrowserActionError::ActionFailed(e.to_string()))?
            }
            "scroll" => {
                let pixels = args.scroll_pixels.unwrap_or(500);
                self.session
                    .scroll(tab, pixels)
                    .await
                    .map_err(|e| BrowserActionError::ActionFailed(e.to_string()))?
            }
            "wait" => {
                let selector = args
                    .selector
                    .ok_or_else(|| BrowserActionError::MissingParam("selector".into()))?;
                let timeout = args.timeout_ms.unwrap_or(5000);
                self.session
                    .wait_for_selector(tab, &selector, timeout)
                    .await
                    .map_err(|e| BrowserActionError::ActionFailed(e.to_string()))?
            }
            other => {
                return Err(BrowserActionError::UnknownAction(other.to_string()));
            }
        };

        // Auto-snapshot after state-changing actions so the LLM sees the new page
        let snapshot = match action_name.as_str() {
            "click" | "fill" | "select" | "scroll" => {
                // Brief wait for page to settle after click (navigation, SPA routing, etc.)
                if action_name == "click" {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                match self
                    .session
                    .build_page_snapshot(tab, &self.login_profiles)
                    .await
                {
                    Ok(snap) => Some(snap),
                    Err(e) => {
                        tracing::debug!(error = ?e, "Auto-snapshot after action failed (non-fatal)");
                        None
                    }
                }
            }
            _ => None,
        };

        Ok(BrowserActionOutput {
            success: true,
            result,
            snapshot,
        })
    }
}
