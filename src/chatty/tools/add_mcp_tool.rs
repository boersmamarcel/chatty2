use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::repositories::McpRepository;

/// Error type for add_mcp tool
#[derive(Debug, thiserror::Error)]
pub enum AddMcpToolError {
    #[error("Validation error: {0}")]
    ValidationError(String),
    #[error("Repository error: {0}")]
    RepositoryError(String),
}

/// Arguments for adding an MCP server
#[derive(Deserialize, Serialize)]
pub struct AddMcpToolArgs {
    /// Unique name for the MCP server (e.g., "tavily-search", "github")
    pub name: String,
    /// Command to execute (e.g., "npx", "uvx", "docker")
    pub command: String,
    /// Command-line arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables for the server process
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Output from the add_mcp tool
#[derive(Debug, Serialize)]
pub struct AddMcpToolOutput {
    pub success: bool,
    pub message: String,
    pub server_name: String,
}

/// Tool that allows the LLM to add MCP server configurations
#[derive(Clone)]
pub struct AddMcpTool {
    repository: Arc<dyn McpRepository>,
}

impl AddMcpTool {
    pub fn new(repository: Arc<dyn McpRepository>) -> Self {
        Self { repository }
    }
}

/// Validate an MCP server configuration, returning an error message if invalid.
fn validate_config(args: &AddMcpToolArgs) -> Result<(), String> {
    // Name must not be empty
    if args.name.trim().is_empty() {
        return Err("Server name cannot be empty".to_string());
    }

    // Name must be reasonable (alphanumeric, hyphens, underscores)
    if !args
        .name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Server name must contain only alphanumeric characters, hyphens, or underscores"
                .to_string(),
        );
    }

    // Command must not be empty
    if args.command.trim().is_empty() {
        return Err("Command cannot be empty".to_string());
    }

    // Env var keys must not be empty
    for key in args.env.keys() {
        if key.trim().is_empty() {
            return Err("Environment variable keys cannot be empty".to_string());
        }
    }

    Ok(())
}

impl Tool for AddMcpTool {
    const NAME: &'static str = "add_mcp_service";
    type Error = AddMcpToolError;
    type Args = AddMcpToolArgs;
    type Output = AddMcpToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "add_mcp_service".to_string(),
            description: "Add a new MCP (Model Context Protocol) server configuration. \
                         This registers a new MCP server that will be available after restarting \
                         the application or creating a new conversation. \
                         \n\n\
                         Use this when the user wants to connect to a new MCP service. \
                         Common examples include:\n\
                         - npx-based servers: command=\"npx\", args=[\"-y\", \"@package/name\"]\n\
                         - uvx-based servers: command=\"uvx\", args=[\"package-name\"]\n\
                         - Docker-based servers: command=\"docker\", args=[\"run\", ...]\n\
                         \n\
                         Environment variables can be used for API keys and configuration. \
                         The server will be enabled by default."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique name identifier for the MCP server (e.g., 'tavily-search', 'github', 'filesystem'). Must contain only alphanumeric characters, hyphens, or underscores."
                    },
                    "command": {
                        "type": "string",
                        "description": "The command to execute to start the MCP server (e.g., 'npx', 'uvx', 'docker', '/usr/local/bin/mcp-server')"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Command-line arguments for the server command (e.g., ['-y', '@anthropic/mcp-server-fetch'])"
                    },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Environment variables to set for the server process (e.g., {'TAVILY_API_KEY': 'tvly-xxxxx'})"
                    }
                },
                "required": ["name", "command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate the configuration
        validate_config(&args).map_err(AddMcpToolError::ValidationError)?;

        let server_name = args.name.clone();

        // Load existing servers
        let mut servers = self.repository.load_all().await.map_err(|e| {
            AddMcpToolError::RepositoryError(format!("Failed to load servers: {}", e))
        })?;

        // Check for duplicate name
        if servers.iter().any(|s| s.name == args.name) {
            return Ok(AddMcpToolOutput {
                success: false,
                message: format!(
                    "An MCP server named '{}' already exists. Choose a different name or remove the existing server first.",
                    args.name
                ),
                server_name,
            });
        }

        // Create the new server config
        let new_server = McpServerConfig {
            name: args.name,
            command: args.command,
            args: args.args,
            env: args.env,
            enabled: true,
        };

        tracing::info!(
            server_name = %new_server.name,
            command = %new_server.command,
            args = ?new_server.args,
            env_keys = ?new_server.env.keys().collect::<Vec<_>>(),
            "Adding new MCP server configuration"
        );

        servers.push(new_server);

        // Save to disk
        self.repository.save_all(servers).await.map_err(|e| {
            AddMcpToolError::RepositoryError(format!("Failed to save servers: {}", e))
        })?;

        Ok(AddMcpToolOutput {
            success: true,
            message: format!(
                "MCP server '{}' has been added successfully. \
                 It will be available after creating a new conversation or restarting the application.",
                server_name
            ),
            server_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_empty_name() {
        let args = AddMcpToolArgs {
            name: "".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
        assert!(
            validate_config(&args)
                .unwrap_err()
                .contains("name cannot be empty")
        );
    }

    #[test]
    fn test_validate_whitespace_name() {
        let args = AddMcpToolArgs {
            name: "   ".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
    }

    #[test]
    fn test_validate_invalid_name_characters() {
        let args = AddMcpToolArgs {
            name: "my server!".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
        assert!(validate_config(&args).unwrap_err().contains("alphanumeric"));
    }

    #[test]
    fn test_validate_valid_name_with_hyphens_and_underscores() {
        let args = AddMcpToolArgs {
            name: "my-server_v2".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_ok());
    }

    #[test]
    fn test_validate_empty_command() {
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            command: "".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
        assert!(
            validate_config(&args)
                .unwrap_err()
                .contains("Command cannot be empty")
        );
    }

    #[test]
    fn test_validate_empty_env_key() {
        let mut env = HashMap::new();
        env.insert("".to_string(), "value".to_string());
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env,
        };
        assert!(validate_config(&args).is_err());
        assert!(
            validate_config(&args)
                .unwrap_err()
                .contains("Environment variable keys")
        );
    }

    #[test]
    fn test_validate_valid_full_config() {
        let mut env = HashMap::new();
        env.insert("API_KEY".to_string(), "test-key".to_string());
        let args = AddMcpToolArgs {
            name: "tavily-search".to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@tavily/mcp-server".to_string()],
            env,
        };
        assert!(validate_config(&args).is_ok());
    }
}
