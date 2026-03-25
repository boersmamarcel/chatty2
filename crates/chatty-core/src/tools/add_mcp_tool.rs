use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::settings::models::mcp_store::{MCP_WRITE_LOCK, McpServerConfig};
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
#[derive(Deserialize, Serialize, Default)]
pub struct AddMcpToolArgs {
    /// Unique name for the MCP server (e.g., "my-tools", "github")
    pub name: String,
    /// HTTP URL of the already-running MCP server endpoint
    /// (e.g., "http://localhost:3000/mcp")
    pub url: String,
    /// Optional Bearer token for authentication (`Authorization: Bearer <api_key>`).
    /// Required for remote servers that enforce access control.
    #[serde(default)]
    pub api_key: Option<String>,
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
    /// Notifies the UI after a successful save. None in tests.
    update_sender: Option<tokio::sync::mpsc::Sender<Vec<McpServerConfig>>>,
}

impl AddMcpTool {
    /// Test constructor: no live services injected.
    pub fn new(repository: Arc<dyn McpRepository>) -> Self {
        Self {
            repository,
            update_sender: None,
        }
    }

    /// Production constructor: inject real sender.
    pub fn new_with_services(
        repository: Arc<dyn McpRepository>,
        update_sender: tokio::sync::mpsc::Sender<Vec<McpServerConfig>>,
        _mcp_service: crate::services::McpService,
    ) -> Self {
        Self {
            repository,
            update_sender: Some(update_sender),
        }
    }
}

/// Validate an MCP server configuration, returning an error message if invalid.
fn validate_config(args: &AddMcpToolArgs) -> Result<(), String> {
    let name = args.name.trim();

    // Name must not be empty
    if name.is_empty() {
        return Err("Server name cannot be empty".to_string());
    }

    // Name must be reasonable (alphanumeric, hyphens, underscores)
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Server name must contain only alphanumeric characters, hyphens, or underscores"
                .to_string(),
        );
    }

    // URL must not be empty
    if args.url.trim().is_empty() {
        return Err("URL cannot be empty".to_string());
    }

    // URL must start with http:// or https://
    let url = args.url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(
            "URL must start with http:// or https:// (e.g. \"http://localhost:3000/mcp\")"
                .to_string(),
        );
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
                         The server must already be running — the app connects to it via HTTP. \
                         The server is ALWAYS saved as disabled — only the user can enable it \
                         via Settings → Execution → MCP Servers. \
                         \n\n\
                         Use this when the user wants to connect to an existing MCP service. \
                         The user is responsible for starting the server before adding it here. \
                         Remote servers typically require an API key sent as a Bearer token. \
                         Local servers usually do not require authentication."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique name identifier for the MCP server (e.g., 'my-tools', 'github', 'filesystem'). Must contain only alphanumeric characters, hyphens, or underscores."
                    },
                    "url": {
                        "type": "string",
                        "description": "HTTP URL of the already-running MCP server endpoint (e.g., 'http://localhost:3000/mcp'). The server must be running before enabling this entry."
                    },
                    "api_key": {
                        "type": "string",
                        "description": "Optional Bearer token for authentication (sent as 'Authorization: Bearer <api_key>'). Required for remote servers that enforce access control. Omit or pass null for unauthenticated local servers."
                    }
                },
                "required": ["name", "url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate before acquiring the lock — read-only and cheap.
        validate_config(&args).map_err(AddMcpToolError::ValidationError)?;

        let server_name = args.name.clone();

        // Acquire write lock: makes load → check → append → save atomic.
        let _guard = MCP_WRITE_LOCK.lock().await;

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

        // Create the new server config. Always disabled — only the user can enable
        // it via Settings after reviewing the configuration.
        let new_server = McpServerConfig {
            name: args.name,
            url: args.url,
            api_key: args.api_key.filter(|k| !k.is_empty()),
            enabled: false,
        };

        tracing::info!(
            server_name = %new_server.name,
            url = %new_server.url,
            has_api_key = new_server.has_api_key(),
            enabled = %new_server.enabled,
            "Adding new MCP server configuration"
        );

        servers.push(new_server);

        // Save to disk inside the lock — critical section ends when save completes.
        self.repository
            .save_all(servers.clone())
            .await
            .map_err(|e| {
                AddMcpToolError::RepositoryError(format!("Failed to save servers: {}", e))
            })?;

        // Release lock before best-effort notification.
        drop(_guard);

        // Notify the UI to refresh via injected sender (None in tests → skipped).
        if let Some(ref tx) = self.update_sender
            && let Err(e) = tx.send(servers).await
        {
            tracing::warn!(error = ?e, "Failed to send MCP update notification");
        }

        let message = format!(
            "MCP server '{}' has been saved as disabled. \
             Enable it in Settings → Execution → MCP Servers once the server is running.",
            server_name
        );

        Ok(AddMcpToolOutput {
            success: true,
            message,
            server_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_helpers::MockMcpRepository;

    /// Helper to create a test McpServerConfig
    fn test_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: true,
        }
    }

    /// Helper to create valid AddMcpToolArgs
    fn valid_args(name: &str) -> AddMcpToolArgs {
        AddMcpToolArgs {
            name: name.to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        }
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_empty_name() {
        let args = AddMcpToolArgs {
            name: "".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
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
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
    }

    #[test]
    fn test_validate_invalid_name_characters() {
        let args = AddMcpToolArgs {
            name: "my server!".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
        assert!(validate_config(&args).unwrap_err().contains("alphanumeric"));
    }

    #[test]
    fn test_validate_name_with_dots() {
        let args = AddMcpToolArgs {
            name: "my.server".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
    }

    #[test]
    fn test_validate_name_with_slashes() {
        let args = AddMcpToolArgs {
            name: "my/server".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
    }

    #[test]
    fn test_validate_valid_name_with_hyphens_and_underscores() {
        let args = AddMcpToolArgs {
            name: "my-server_v2".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_ok());
    }

    #[test]
    fn test_validate_valid_name_alphanumeric_only() {
        let args = AddMcpToolArgs {
            name: "myserver123".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_ok());
    }

    #[test]
    fn test_validate_empty_url() {
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            url: "".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
        assert!(
            validate_config(&args)
                .unwrap_err()
                .contains("URL cannot be empty")
        );
    }

    #[test]
    fn test_validate_whitespace_url() {
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            url: "   ".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
    }

    #[test]
    fn test_validate_url_without_scheme() {
        let args = AddMcpToolArgs {
            name: "test-server".to_string(),
            url: "localhost:3000/mcp".to_string(),
            api_key: None,
        };
        assert!(validate_config(&args).is_err());
        assert!(validate_config(&args).unwrap_err().contains("http://"));
    }

    #[test]
    fn test_validate_https_url_is_valid() {
        let args = AddMcpToolArgs {
            name: "remote-server".to_string(),
            url: "https://mcp.example.com/tools".to_string(),
            api_key: Some("bearer-token".to_string()),
        };
        assert!(validate_config(&args).is_ok());
    }

    #[test]
    fn test_validate_http_url_is_valid() {
        let args = AddMcpToolArgs {
            name: "local-server".to_string(),
            url: "http://localhost:8080/mcp".to_string(),
            api_key: None,
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
        assert!(output.message.contains("MCP server 'new-server' has been"));

        // Verify the server was saved
        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "new-server");
        assert_eq!(saved[0].url, "http://localhost:3000/mcp");
        assert!(saved[0].api_key.is_none());
        assert!(!saved[0].enabled); // disabled by default — user enables via Settings
    }

    #[tokio::test]
    async fn test_call_add_with_api_key() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let args = AddMcpToolArgs {
            name: "remote-server".to_string(),
            url: "https://mcp.example.com/tools".to_string(),
            api_key: Some("sk-secret-bearer-token".to_string()),
        };

        let result = tool.call(args).await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].api_key.as_deref(), Some("sk-secret-bearer-token"));
    }

    #[tokio::test]
    async fn test_call_empty_api_key_stored_as_none() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let args = AddMcpToolArgs {
            name: "local-server".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: Some("".to_string()), // empty → stored as None
        };

        let result = tool.call(args).await;
        assert!(result.is_ok());

        let saved = repo.get_last_saved().unwrap();
        assert!(saved[0].api_key.is_none());
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
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
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
    async fn test_call_new_server_is_disabled_by_default() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo.clone());

        let result = tool.call(valid_args("test-server")).await;
        assert!(result.is_ok());

        let saved = repo.get_last_saved().unwrap();
        assert!(!saved[0].enabled); // disabled by default — user enables via Settings
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
        assert!(required_names.contains(&"url"));
    }

    #[tokio::test]
    async fn test_definition_has_all_properties() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = AddMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("url"));
    }

    // --- Serde deserialization tests ---

    #[test]
    fn test_args_deserialize_minimal() {
        let json = r#"{"name": "test", "url": "http://localhost:3000/mcp"}"#;
        let args: AddMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert_eq!(args.url, "http://localhost:3000/mcp");
        assert!(args.api_key.is_none());
    }

    #[test]
    fn test_args_deserialize_with_api_key() {
        let json =
            r#"{"name": "test", "url": "https://example.com/mcp", "api_key": "bearer-token-123"}"#;
        let args: AddMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.api_key.unwrap(), "bearer-token-123");
    }

    #[test]
    fn test_args_deserialize_missing_name_fails() {
        let json = r#"{"url": "http://localhost:3000/mcp"}"#;
        let result: Result<AddMcpToolArgs, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_args_deserialize_missing_url_fails() {
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
