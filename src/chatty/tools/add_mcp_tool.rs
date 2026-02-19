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
    use crate::settings::repositories::mcp_repository::BoxFuture;
    use crate::settings::repositories::provider_repository::{RepositoryError, RepositoryResult};
    use std::sync::Mutex;

    // --- Mock repository for testing ---

    /// In-memory mock of McpRepository for unit tests
    struct MockMcpRepository {
        servers: Mutex<Vec<McpServerConfig>>,
        /// If set, load_all will return this error
        load_error: Mutex<Option<String>>,
        /// If set, save_all will return this error
        save_error: Mutex<Option<String>>,
        /// Track what was last saved
        last_saved: Mutex<Option<Vec<McpServerConfig>>>,
    }

    impl MockMcpRepository {
        fn new() -> Self {
            Self {
                servers: Mutex::new(Vec::new()),
                load_error: Mutex::new(None),
                save_error: Mutex::new(None),
                last_saved: Mutex::new(None),
            }
        }

        fn with_servers(servers: Vec<McpServerConfig>) -> Self {
            Self {
                servers: Mutex::new(servers),
                load_error: Mutex::new(None),
                save_error: Mutex::new(None),
                last_saved: Mutex::new(None),
            }
        }

        fn with_load_error(error: &str) -> Self {
            Self {
                servers: Mutex::new(Vec::new()),
                load_error: Mutex::new(Some(error.to_string())),
                save_error: Mutex::new(None),
                last_saved: Mutex::new(None),
            }
        }

        fn with_save_error(servers: Vec<McpServerConfig>, error: &str) -> Self {
            Self {
                servers: Mutex::new(servers),
                load_error: Mutex::new(None),
                save_error: Mutex::new(Some(error.to_string())),
                last_saved: Mutex::new(None),
            }
        }

        fn get_last_saved(&self) -> Option<Vec<McpServerConfig>> {
            self.last_saved.lock().unwrap().clone()
        }
    }

    impl McpRepository for MockMcpRepository {
        fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<McpServerConfig>>> {
            let servers = self.servers.lock().unwrap().clone();
            let error = self.load_error.lock().unwrap().clone();
            Box::pin(async move {
                if let Some(err) = error {
                    Err(RepositoryError::IoError(err))
                } else {
                    Ok(servers)
                }
            })
        }

        fn save_all(
            &self,
            servers: Vec<McpServerConfig>,
        ) -> BoxFuture<'static, RepositoryResult<()>> {
            let error = self.save_error.lock().unwrap().clone();
            *self.last_saved.lock().unwrap() = Some(servers);
            Box::pin(async move {
                if let Some(err) = error {
                    Err(RepositoryError::IoError(err))
                } else {
                    Ok(())
                }
            })
        }
    }

    /// Helper to create a test McpServerConfig
    fn test_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            env: HashMap::new(),
            enabled: true,
        }
    }

    /// Helper to create valid AddMcpToolArgs
    fn valid_args(name: &str) -> AddMcpToolArgs {
        AddMcpToolArgs {
            name: name.to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@test/mcp-server".to_string()],
            env: HashMap::new(),
        }
    }

    // --- Validation tests ---

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
    fn test_validate_name_with_dots() {
        let args = AddMcpToolArgs {
            name: "my.server".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
    }

    #[test]
    fn test_validate_name_with_slashes() {
        let args = AddMcpToolArgs {
            name: "my/server".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
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
    fn test_validate_valid_name_alphanumeric_only() {
        let args = AddMcpToolArgs {
            name: "myserver123".to_string(),
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
    fn test_validate_whitespace_command() {
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            command: "   ".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_err());
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
    fn test_validate_whitespace_env_key() {
        let mut env = HashMap::new();
        env.insert("  ".to_string(), "value".to_string());
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env,
        };
        assert!(validate_config(&args).is_err());
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

    #[test]
    fn test_validate_empty_args_and_env_is_valid() {
        let args = AddMcpToolArgs {
            name: "simple".to_string(),
            command: "my-server".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        assert!(validate_config(&args).is_ok());
    }

    #[test]
    fn test_validate_multiple_env_vars() {
        let mut env = HashMap::new();
        env.insert("KEY1".to_string(), "val1".to_string());
        env.insert("KEY2".to_string(), "val2".to_string());
        env.insert("KEY3".to_string(), "val3".to_string());
        let args = AddMcpToolArgs {
            name: "multi-env".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env,
        };
        assert!(validate_config(&args).is_ok());
    }

    // --- Tool::call integration tests ---

    #[tokio::test]
    async fn test_call_add_to_empty_repo() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let result = tool.call(valid_args("new-server")).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.success);
        assert_eq!(output.server_name, "new-server");
        assert!(output.message.contains("added successfully"));

        // Verify the server was saved
        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "new-server");
        assert_eq!(saved[0].command, "npx");
        assert_eq!(saved[0].args, vec!["-y", "@test/mcp-server"]);
        assert!(saved[0].enabled);
    }

    #[tokio::test]
    async fn test_call_add_to_existing_servers() {
        let existing = vec![test_server("existing-1"), test_server("existing-2")];
        let repo = Arc::new(MockMcpRepository::with_servers(existing));
        let tool = AddMcpTool::new(repo.clone());

        let result = tool.call(valid_args("new-server")).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.success);

        // Verify all servers are preserved in the save
        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved.len(), 3);
        assert_eq!(saved[0].name, "existing-1");
        assert_eq!(saved[1].name, "existing-2");
        assert_eq!(saved[2].name, "new-server");
    }

    #[tokio::test]
    async fn test_call_duplicate_name_returns_failure() {
        let existing = vec![test_server("my-server")];
        let repo = Arc::new(MockMcpRepository::with_servers(existing));
        let tool = AddMcpTool::new(repo.clone());

        let result = tool.call(valid_args("my-server")).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.success);
        assert_eq!(output.server_name, "my-server");
        assert!(output.message.contains("already exists"));

        // Verify nothing was saved (duplicate rejected before save)
        assert!(repo.get_last_saved().is_none());
    }

    #[tokio::test]
    async fn test_call_validation_error_returns_err() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let args = AddMcpToolArgs {
            name: "".to_string(),
            command: "npx".to_string(),
            args: vec![],
            env: HashMap::new(),
        };

        let result = tool.call(args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AddMcpToolError::ValidationError(_)));
        assert!(err.to_string().contains("Validation error"));
    }

    #[tokio::test]
    async fn test_call_load_error() {
        let repo = Arc::new(MockMcpRepository::with_load_error("disk read failure"));
        let tool = AddMcpTool::new(repo);

        let result = tool.call(valid_args("new-server")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AddMcpToolError::RepositoryError(_)));
        assert!(err.to_string().contains("Failed to load servers"));
    }

    #[tokio::test]
    async fn test_call_save_error() {
        let repo = Arc::new(MockMcpRepository::with_save_error(
            vec![],
            "disk write failure",
        ));
        let tool = AddMcpTool::new(repo);

        let result = tool.call(valid_args("new-server")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AddMcpToolError::RepositoryError(_)));
        assert!(err.to_string().contains("Failed to save servers"));
    }

    #[tokio::test]
    async fn test_call_preserves_env_vars() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let mut env = HashMap::new();
        env.insert("API_KEY".to_string(), "secret-123".to_string());
        env.insert("REGION".to_string(), "us-east-1".to_string());

        let args = AddMcpToolArgs {
            name: "env-server".to_string(),
            command: "uvx".to_string(),
            args: vec!["my-package".to_string()],
            env,
        };

        let result = tool.call(args).await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].env.get("API_KEY").unwrap(), "secret-123");
        assert_eq!(saved[0].env.get("REGION").unwrap(), "us-east-1");
        assert_eq!(saved[0].command, "uvx");
        assert_eq!(saved[0].args, vec!["my-package"]);
    }

    #[tokio::test]
    async fn test_call_new_server_is_enabled_by_default() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let result = tool.call(valid_args("test-server")).await;
        assert!(result.is_ok());

        let saved = repo.get_last_saved().unwrap();
        assert!(saved[0].enabled);
    }

    // --- Tool definition tests ---

    #[tokio::test]
    async fn test_definition_has_correct_name() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "add_mcp_service");
    }

    #[tokio::test]
    async fn test_definition_has_required_fields() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let required = def.parameters["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"name"));
        assert!(required_names.contains(&"command"));
        // args and env should be optional (not in required)
        assert!(!required_names.contains(&"args"));
        assert!(!required_names.contains(&"env"));
    }

    #[tokio::test]
    async fn test_definition_has_all_properties() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("command"));
        assert!(props.contains_key("args"));
        assert!(props.contains_key("env"));
    }

    // --- Serde deserialization tests ---

    #[test]
    fn test_args_deserialize_minimal() {
        let json = r#"{"name": "test", "command": "npx"}"#;
        let args: AddMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert_eq!(args.command, "npx");
        assert!(args.args.is_empty());
        assert!(args.env.is_empty());
    }

    #[test]
    fn test_args_deserialize_full() {
        let json = r#"{
            "name": "tavily",
            "command": "npx",
            "args": ["-y", "@tavily/mcp-server"],
            "env": {"TAVILY_API_KEY": "tvly-xxx"}
        }"#;
        let args: AddMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "tavily");
        assert_eq!(args.command, "npx");
        assert_eq!(args.args, vec!["-y", "@tavily/mcp-server"]);
        assert_eq!(args.env.get("TAVILY_API_KEY").unwrap(), "tvly-xxx");
    }

    #[test]
    fn test_args_deserialize_missing_name_fails() {
        let json = r#"{"command": "npx"}"#;
        let result: Result<AddMcpToolArgs, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_args_deserialize_missing_command_fails() {
        let json = r#"{"name": "test"}"#;
        let result: Result<AddMcpToolArgs, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_output_serialization() {
        let output = AddMcpToolOutput {
            success: true,
            message: "Added successfully".to_string(),
            server_name: "test".to_string(),
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"server_name\":\"test\""));
    }

    // --- Error display tests ---

    #[test]
    fn test_validation_error_display() {
        let err = AddMcpToolError::ValidationError("bad name".to_string());
        assert_eq!(err.to_string(), "Validation error: bad name");
    }

    #[test]
    fn test_repository_error_display() {
        let err = AddMcpToolError::RepositoryError("disk full".to_string());
        assert_eq!(err.to_string(), "Repository error: disk full");
    }

    // --- Tool constant tests ---

    #[test]
    fn test_tool_name_constant() {
        assert_eq!(AddMcpTool::NAME, "add_mcp_service");
    }
}
