use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::a2a_store::{A2aAgentConfig, A2aAgentStatus};
use super::mcp_store::{McpAuthStatus, McpServerConfig};

// ── Extension source ───────────────────────────────────────────────────────

/// Where this extension was installed from.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ExtensionSource {
    /// Installed from the Hive marketplace.
    #[serde(rename = "hive")]
    Hive {
        module_name: String,
        version: String,
    },
    /// Manually configured by the user.
    #[serde(rename = "custom")]
    Custom,
}

// ── Extension kind ─────────────────────────────────────────────────────────

/// The technical type of the extension — determines how it is activated.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ExtensionKind {
    /// A remote MCP server (stdio or SSE).
    #[serde(rename = "mcp")]
    McpServer(McpServerConfig),
    /// A local WASM module (loaded by the module runtime).
    #[serde(rename = "wasm")]
    WasmModule,
    /// A remote A2A agent endpoint.
    #[serde(rename = "a2a")]
    A2aAgent(A2aAgentConfig),
}

// ── Installed extension ────────────────────────────────────────────────────

/// A single installed extension, regardless of its technical type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledExtension {
    /// Unique slug (e.g. `"github-mcp"` or `"readability-auditor"`).
    pub id: String,
    /// Human-readable name shown in the UI.
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// The technical backing of this extension.
    pub kind: ExtensionKind,
    /// Where this extension came from.
    pub source: ExtensionSource,
    /// Marketplace pricing classification for Hive-sourced modules.
    #[serde(default)]
    pub pricing_model: Option<String>,
    /// Whether the extension is currently active.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

// ── Store ──────────────────────────────────────────────────────────────────

/// Global store for all installed extensions (MCP, WASM, A2A).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ExtensionsModel {
    #[serde(default)]
    pub extensions: Vec<InstalledExtension>,

    /// Runtime auth status per MCP server name (not persisted).
    #[serde(skip)]
    mcp_auth_statuses: HashMap<String, McpAuthStatus>,

    /// Runtime connection status per A2A agent name (not persisted).
    #[serde(skip)]
    a2a_statuses: HashMap<String, A2aAgentStatus>,
}

impl ExtensionsModel {
    /// Return all enabled MCP server configs (borrowed).
    pub fn mcp_servers(&self) -> Vec<&McpServerConfig> {
        self.extensions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| match &e.kind {
                ExtensionKind::McpServer(cfg) => Some(cfg),
                _ => None,
            })
            .collect()
    }

    /// Return all enabled A2A agent configs (borrowed).
    pub fn a2a_agents(&self) -> Vec<&A2aAgentConfig> {
        self.extensions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some(cfg),
                _ => None,
            })
            .collect()
    }

    /// Return IDs of all enabled WASM module extensions.
    pub fn wasm_module_ids(&self) -> Vec<&str> {
        self.extensions
            .iter()
            .filter(|e| e.enabled && matches!(e.kind, ExtensionKind::WasmModule))
            .map(|e| e.id.as_str())
            .collect()
    }

    pub fn find(&self, id: &str) -> Option<&InstalledExtension> {
        self.extensions.iter().find(|e| e.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut InstalledExtension> {
        self.extensions.iter_mut().find(|e| e.id == id)
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.extensions.iter().any(|e| e.id == id)
    }

    pub fn add(&mut self, ext: InstalledExtension) {
        if !self.is_installed(&ext.id) {
            self.extensions.push(ext);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.extensions.retain(|e| e.id != id);
    }

    // ── MCP convenience methods ────────────────────────────────────────────

    /// Return all MCP server configs (regardless of enabled state).
    pub fn all_mcp_servers(&self) -> Vec<(String, McpServerConfig, bool)> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::McpServer(cfg) => Some((e.id.clone(), cfg.clone(), e.enabled)),
                _ => None,
            })
            .collect()
    }

    /// Find an MCP extension by its server name.
    pub fn find_mcp_by_name(&self, name: &str) -> Option<&InstalledExtension> {
        self.extensions.iter().find(|e| match &e.kind {
            ExtensionKind::McpServer(cfg) => cfg.name == name,
            _ => false,
        })
    }

    /// Find an MCP extension mutably by its server name.
    pub fn find_mcp_by_name_mut(&mut self, name: &str) -> Option<&mut InstalledExtension> {
        self.extensions.iter_mut().find(|e| match &e.kind {
            ExtensionKind::McpServer(cfg) => cfg.name == name,
            _ => false,
        })
    }

    /// Remove an MCP extension by its server name.
    pub fn remove_mcp_by_name(&mut self, name: &str) {
        self.extensions.retain(|e| match &e.kind {
            ExtensionKind::McpServer(cfg) => cfg.name != name,
            _ => true,
        });
    }

    /// Count of enabled MCP server extensions.
    pub fn enabled_mcp_count(&self) -> usize {
        self.extensions
            .iter()
            .filter(|e| e.enabled && matches!(e.kind, ExtensionKind::McpServer(_)))
            .count()
    }

    /// Get the runtime auth status for an MCP server.
    pub fn mcp_auth_status(&self, server_name: &str) -> &McpAuthStatus {
        self.mcp_auth_statuses
            .get(server_name)
            .unwrap_or(&McpAuthStatus::NotRequired)
    }

    /// Set the runtime auth status for an MCP server.
    pub fn set_mcp_auth_status(&mut self, server_name: String, status: McpAuthStatus) {
        self.mcp_auth_statuses.insert(server_name, status);
    }

    /// Remove the runtime auth status for an MCP server.
    pub fn remove_mcp_auth_status(&mut self, server_name: &str) {
        self.mcp_auth_statuses.remove(server_name);
    }

    // ── A2A convenience methods ────────────────────────────────────────────

    /// Return all A2A agent configs (regardless of enabled state).
    pub fn all_a2a_agents(&self) -> Vec<(String, A2aAgentConfig, bool)> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some((e.id.clone(), cfg.clone(), e.enabled)),
                _ => None,
            })
            .collect()
    }

    /// Return just the A2A agent configs (for agent_factory compatibility).
    pub fn a2a_agent_configs(&self) -> Vec<A2aAgentConfig> {
        self.extensions
            .iter()
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some(cfg.clone()),
                _ => None,
            })
            .collect()
    }

    /// Find an A2A extension by its agent name.
    pub fn find_a2a_by_name(&self, name: &str) -> Option<&InstalledExtension> {
        self.extensions.iter().find(|e| match &e.kind {
            ExtensionKind::A2aAgent(cfg) => cfg.name == name,
            _ => false,
        })
    }

    /// Find an A2A extension mutably by its agent name.
    pub fn find_a2a_by_name_mut(&mut self, name: &str) -> Option<&mut InstalledExtension> {
        self.extensions.iter_mut().find(|e| match &e.kind {
            ExtensionKind::A2aAgent(cfg) => cfg.name == name,
            _ => false,
        })
    }

    /// Remove an A2A extension by its agent name.
    pub fn remove_a2a_by_name(&mut self, name: &str) {
        self.extensions.retain(|e| match &e.kind {
            ExtensionKind::A2aAgent(cfg) => cfg.name != name,
            _ => true,
        });
    }

    /// Look up an enabled A2A agent config by name.
    pub fn find_enabled_a2a(&self, name: &str) -> Option<&A2aAgentConfig> {
        self.extensions.iter().find_map(|e| match &e.kind {
            ExtensionKind::A2aAgent(cfg) if e.enabled && cfg.name == name => Some(cfg),
            _ => None,
        })
    }

    /// Count of enabled A2A agent extensions.
    pub fn enabled_a2a_count(&self) -> usize {
        self.extensions
            .iter()
            .filter(|e| e.enabled && matches!(e.kind, ExtensionKind::A2aAgent(_)))
            .count()
    }

    /// Get the runtime connection status for an A2A agent.
    pub fn a2a_status(&self, agent_name: &str) -> &A2aAgentStatus {
        self.a2a_statuses
            .get(agent_name)
            .unwrap_or(&A2aAgentStatus::Unknown)
    }

    /// Set the runtime connection status for an A2A agent.
    pub fn set_a2a_status(&mut self, agent_name: String, status: A2aAgentStatus) {
        self.a2a_statuses.insert(agent_name, status);
    }

    /// Remove the runtime connection status for an A2A agent.
    pub fn remove_a2a_status(&mut self, agent_name: &str) {
        self.a2a_statuses.remove(agent_name);
    }

    // ── Generic helpers ────────────────────────────────────────────────────

    /// Toggle the enabled flag on an extension. Returns the new enabled state,
    /// or `None` if the extension was not found.
    pub fn toggle_extension(&mut self, id: &str) -> Option<bool> {
        if let Some(ext) = self.find_mut(id) {
            ext.enabled = !ext.enabled;
            Some(ext.enabled)
        } else {
            None
        }
    }
}
