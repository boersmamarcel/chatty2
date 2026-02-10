use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a single MCP server
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name identifier for the MCP server
    pub name: String,

    /// Command to execute (e.g., "npx", "uvx", "/usr/local/bin/mcp-server")
    pub command: String,

    /// Command-line arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set for the process
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Whether this server is enabled/active
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Global store for MCP server configurations
#[derive(Clone)]
pub struct McpServersModel {
    servers: Vec<McpServerConfig>,
}

impl Global for McpServersModel {}

impl McpServersModel {
    pub fn new() -> Self {
        Self {
            servers: Vec::new(),
        }
    }

    pub fn servers(&self) -> &[McpServerConfig] {
        &self.servers
    }

    /// Replace all servers (used when loading from disk)
    pub fn replace_all(&mut self, servers: Vec<McpServerConfig>) {
        self.servers = servers;
    }
}

impl Default for McpServersModel {
    fn default() -> Self {
        Self::new()
    }
}
