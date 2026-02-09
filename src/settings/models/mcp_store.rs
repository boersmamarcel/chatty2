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
    
    /// Transport type (only "stdio" supported in MVP)
    #[serde(default = "default_transport")]
    pub transport: String,
    
    /// Whether this server is enabled/active
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_transport() -> String {
    "stdio".to_string()
}

fn default_enabled() -> bool {
    true
}

impl McpServerConfig {
    pub fn new(name: String, command: String, args: Vec<String>) -> Self {
        Self {
            name,
            command,
            args,
            env: HashMap::new(),
            transport: default_transport(),
            enabled: true,
        }
    }

    pub fn with_env(mut self, key: String, value: String) -> Self {
        self.env.insert(key, value);
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
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

    pub fn add_server(&mut self, config: McpServerConfig) {
        self.servers.push(config);
    }

    pub fn servers(&self) -> &[McpServerConfig] {
        &self.servers
    }

    pub fn servers_mut(&mut self) -> &mut Vec<McpServerConfig> {
        &mut self.servers
    }

    /// Replace all servers (used when loading from disk)
    pub fn replace_all(&mut self, servers: Vec<McpServerConfig>) {
        self.servers = servers;
    }

    /// Get only enabled servers
    pub fn enabled_servers(&self) -> Vec<&McpServerConfig> {
        self.servers
            .iter()
            .filter(|s| s.enabled)
            .collect()
    }

    /// Find a server by name
    pub fn get_server(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Remove a server by name
    pub fn remove_server(&mut self, name: &str) -> bool {
        if let Some(pos) = self.servers.iter().position(|s| s.name == name) {
            self.servers.remove(pos);
            true
        } else {
            false
        }
    }

    /// Update an existing server configuration
    pub fn update_server(&mut self, name: &str, config: McpServerConfig) -> bool {
        if let Some(server) = self.servers.iter_mut().find(|s| s.name == name) {
            *server = config;
            true
        } else {
            false
        }
    }
}

impl Default for McpServersModel {
    fn default() -> Self {
        Self::new()
    }
}
