use serde::{Deserialize, Serialize};
use std::collections::HashMap;

lazy_static::lazy_static! {
    /// Shared write lock for all MCP tool operations (add, delete, edit).
    ///
    /// Serialises concurrent MCP tool calls so the load → modify → save
    /// sequence is atomic, preventing TOCTOU races across different tools.
    pub static ref MCP_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

/// Sentinel value used to represent a masked (hidden) API key value.
///
/// When the LLM sends this value back in `edit_mcp_service`, the original
/// stored value is preserved rather than overwriting with the literal string.
pub const MASKED_API_KEY_SENTINEL: &str = "****";

/// Runtime authentication status for an MCP server.
/// Not persisted — derived from connection state and cached credentials.
#[derive(Clone, Debug, PartialEq)]
pub enum McpAuthStatus {
    /// No special auth needed, or using static API key
    NotRequired,
    /// OAuth tokens are cached and server is connected
    Authenticated,
    /// Server requires OAuth but no cached tokens exist
    NeedsAuth,
    /// OAuth flow or connection in progress
    Connecting,
    /// Auth or connection failed
    Failed(String),
}

/// Configuration for a single MCP server.
///
/// The app connects to servers that are already running — either locally or
/// remotely. It is the user's responsibility to start the server before adding
/// it here. The `url` field must point to the server's MCP endpoint (e.g.
/// `http://localhost:3000/mcp` for a streamable-HTTP server).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name identifier for the MCP server
    pub name: String,

    /// HTTP URL of the already-running MCP server endpoint
    /// (e.g. "http://localhost:3000/mcp")
    pub url: String,

    /// Optional Bearer token sent as `Authorization: Bearer <api_key>`.
    /// Used for remote MCP servers that require authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Whether this server is enabled/active
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl McpServerConfig {
    /// Returns true if an API key has been configured.
    pub fn has_api_key(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }
}

/// Global store for MCP server configurations
#[derive(Clone)]
pub struct McpServersModel {
    servers: Vec<McpServerConfig>,
    /// Runtime auth status per server (not persisted)
    auth_statuses: HashMap<String, McpAuthStatus>,
}

impl McpServersModel {
    pub fn new() -> Self {
        Self {
            servers: Vec::new(),
            auth_statuses: HashMap::new(),
        }
    }

    pub fn servers(&self) -> &[McpServerConfig] {
        &self.servers
    }

    /// Get mutable access to servers (for in-place updates)
    pub fn servers_mut(&mut self) -> &mut Vec<McpServerConfig> {
        &mut self.servers
    }

    /// Count enabled servers
    pub fn enabled_count(&self) -> usize {
        self.servers.iter().filter(|s| s.enabled).count()
    }

    /// Replace all servers (used when loading from disk)
    pub fn replace_all(&mut self, servers: Vec<McpServerConfig>) {
        self.servers = servers;
    }

    /// Get auth status for a server
    pub fn auth_status(&self, server_name: &str) -> &McpAuthStatus {
        self.auth_statuses
            .get(server_name)
            .unwrap_or(&McpAuthStatus::NotRequired)
    }

    /// Set auth status for a server
    pub fn set_auth_status(&mut self, server_name: String, status: McpAuthStatus) {
        self.auth_statuses.insert(server_name, status);
    }

    /// Remove auth status for a server (e.g. on delete)
    pub fn remove_auth_status(&mut self, server_name: &str) {
        self.auth_statuses.remove(server_name);
    }
}

impl Default for McpServersModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_server_config_serialization() {
        let config = McpServerConfig {
            name: "test-server".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"name\":\"test-server\""));
        assert!(json.contains("\"url\":\"http://localhost:3000/mcp\""));
        assert!(json.contains("\"enabled\":true"));
        // api_key is skipped when None
        assert!(!json.contains("api_key"));
    }

    #[test]
    fn test_mcp_server_config_with_api_key_serialization() {
        let config = McpServerConfig {
            name: "remote-server".to_string(),
            url: "https://mcp.example.com/tools".to_string(),
            api_key: Some("sk-secret-token".to_string()),
            enabled: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"api_key\":\"sk-secret-token\""));
    }

    #[test]
    fn test_mcp_server_config_deserialization() {
        let json = r#"{"name":"my-server","url":"http://localhost:8080/mcp","enabled":false}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "my-server");
        assert_eq!(config.url, "http://localhost:8080/mcp");
        assert!(config.api_key.is_none());
        assert!(!config.enabled);
    }

    #[test]
    fn test_mcp_server_config_with_api_key_deserialization() {
        let json = r#"{"name":"test","url":"http://localhost:3000/mcp","api_key":"bearer-token-123","enabled":true}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.api_key.unwrap(), "bearer-token-123");
    }

    #[test]
    fn test_default_enabled_is_true() {
        let json = r#"{"name":"test","url":"http://localhost:3000/mcp"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
    }

    #[test]
    fn test_has_api_key() {
        let with_key = McpServerConfig {
            name: "a".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: Some("token".to_string()),
            enabled: true,
        };
        assert!(with_key.has_api_key());

        let without_key = McpServerConfig {
            name: "b".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: true,
        };
        assert!(!without_key.has_api_key());

        let empty_key = McpServerConfig {
            name: "c".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: Some("".to_string()),
            enabled: true,
        };
        assert!(!empty_key.has_api_key());
    }

    #[test]
    fn test_mcp_servers_model_enabled_count() {
        let mut model = McpServersModel::new();
        model.replace_all(vec![
            McpServerConfig {
                name: "a".to_string(),
                url: "http://localhost:3001/mcp".to_string(),
                api_key: None,
                enabled: true,
            },
            McpServerConfig {
                name: "b".to_string(),
                url: "http://localhost:3002/mcp".to_string(),
                api_key: None,
                enabled: false,
            },
            McpServerConfig {
                name: "c".to_string(),
                url: "http://localhost:3003/mcp".to_string(),
                api_key: Some("token".to_string()),
                enabled: true,
            },
        ]);
        assert_eq!(model.enabled_count(), 2);
    }
}
