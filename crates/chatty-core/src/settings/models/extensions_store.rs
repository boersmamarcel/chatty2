use serde::{Deserialize, Serialize};

use super::a2a_store::A2aAgentConfig;
use super::mcp_store::McpServerConfig;

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
}

impl ExtensionsModel {
    /// Return all enabled MCP server configs.
    pub fn mcp_servers(&self) -> Vec<McpServerConfig> {
        self.extensions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| match &e.kind {
                ExtensionKind::McpServer(cfg) => Some(cfg.clone()),
                _ => None,
            })
            .collect()
    }

    /// Return all enabled A2A agent configs.
    pub fn a2a_agents(&self) -> Vec<A2aAgentConfig> {
        self.extensions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| match &e.kind {
                ExtensionKind::A2aAgent(cfg) => Some(cfg.clone()),
                _ => None,
            })
            .collect()
    }

    /// Return IDs of all enabled WASM module extensions.
    pub fn wasm_module_ids(&self) -> Vec<String> {
        self.extensions
            .iter()
            .filter(|e| e.enabled && matches!(e.kind, ExtensionKind::WasmModule))
            .map(|e| e.id.clone())
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
}
