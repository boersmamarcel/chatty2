use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

lazy_static::lazy_static! {
    /// Shared write lock for all MCP tool operations (add, delete, edit).
    ///
    /// Serialises concurrent MCP tool calls so the load → modify → save
    /// sequence is atomic, preventing TOCTOU races across different tools.
    pub static ref MCP_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

/// Sentinel value used to represent a masked (hidden) env var value.
///
/// When the LLM sends this value back in `edit_mcp_service`, the original
/// stored value is preserved rather than overwriting with the literal string.
pub const MASKED_VALUE_SENTINEL: &str = "****";

/// Returns true if the key name suggests a sensitive value.
///
/// Matches keys where any `_`-delimited segment exactly equals KEY, SECRET,
/// TOKEN, PASSWORD, CREDENTIAL, AUTH, or API (case-insensitive). Whole-word
/// matching avoids false positives like MONKEY (contains KEY) or WORKPATH.
pub fn is_sensitive_env_key(key: &str) -> bool {
    const SENSITIVE_WORDS: &[&str] = &[
        "KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "CREDENTIAL",
        "AUTH",
        "API",
    ];
    key.split('_')
        .any(|segment| SENSITIVE_WORDS.contains(&segment.to_uppercase().as_str()))
}

/// Masks a sensitive env var value. Non-sensitive values are returned as-is.
///
/// Sensitive values are fully replaced with `MASKED_VALUE_SENTINEL` (`"****"`).
pub fn mask_env_value(key: &str, value: &str) -> String {
    if is_sensitive_env_key(key) {
        MASKED_VALUE_SENTINEL.to_string()
    } else {
        value.to_string()
    }
}

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

impl McpServerConfig {
    /// Returns env vars with sensitive values masked, safe for display to LLMs.
    ///
    /// Sensitive keys (containing KEY, SECRET, TOKEN, etc.) have their values
    /// replaced with `"****"`. Non-sensitive keys are returned as-is.
    pub fn masked_env(&self) -> HashMap<String, String> {
        self.env
            .iter()
            .map(|(k, v)| (k.clone(), mask_env_value(k, v)))
            .collect()
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
    fn test_is_sensitive_env_key() {
        // True positives — whole-word sensitive segments
        assert!(is_sensitive_env_key("TAVILY_API_KEY"));
        assert!(is_sensitive_env_key("GITHUB_TOKEN"));
        assert!(is_sensitive_env_key("AWS_SECRET_ACCESS_KEY"));
        assert!(is_sensitive_env_key("PASSWORD"));
        assert!(is_sensitive_env_key("AUTH_HEADER"));
        assert!(is_sensitive_env_key("api_key")); // lowercase
        assert!(is_sensitive_env_key("API"));
        assert!(is_sensitive_env_key("KEY"));
        assert!(is_sensitive_env_key("SECRET"));
        assert!(is_sensitive_env_key("TOKEN"));
        assert!(is_sensitive_env_key("CREDENTIAL"));
        assert!(is_sensitive_env_key("MY_API"));

        // True negatives — non-sensitive keys
        assert!(!is_sensitive_env_key("HOST"));
        assert!(!is_sensitive_env_key("PORT"));
        assert!(!is_sensitive_env_key("DEBUG"));
        assert!(!is_sensitive_env_key("WORKSPACE"));

        // No more false positives from substring matching
        assert!(!is_sensitive_env_key("MONKEY_HOST")); // contains KEY as substring
        assert!(!is_sensitive_env_key("TURKEY")); // contains KEY as substring
        assert!(!is_sensitive_env_key("WORKPATH")); // does not match PATH
    }

    #[test]
    fn test_mask_env_value() {
        assert_eq!(mask_env_value("TAVILY_API_KEY", "tvly-abc123"), "****");
        assert_eq!(mask_env_value("GITHUB_TOKEN", "ghp_abcdefgh"), "****");
        assert_eq!(mask_env_value("PASSWORD", "hunter2"), "****");
        assert_eq!(mask_env_value("HOST", "localhost"), "localhost");
        assert_eq!(mask_env_value("PORT", "8080"), "8080");
    }

    #[test]
    fn test_masked_env() {
        let config = McpServerConfig {
            name: "test".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::from([
                ("TAVILY_API_KEY".to_string(), "tvly-real-key".to_string()),
                ("HOST".to_string(), "localhost".to_string()),
                ("GITHUB_TOKEN".to_string(), "ghp_secret".to_string()),
            ]),
            enabled: true,
        };
        let masked = config.masked_env();
        assert_eq!(masked["TAVILY_API_KEY"], "****");
        assert_eq!(masked["HOST"], "localhost");
        assert_eq!(masked["GITHUB_TOKEN"], "****");
    }
}
