use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::settings::models::mcp_store::{
    MASKED_API_KEY_SENTINEL, MCP_WRITE_LOCK, McpServerConfig,
};
use crate::settings::repositories::McpRepository;

/// Error type for edit_mcp tool
#[derive(Debug, thiserror::Error)]
pub enum EditMcpToolError {
    #[error("Validation error: {0}")]
    ValidationError(String),
    #[error("Repository error: {0}")]
    RepositoryError(String),
}

/// Arguments for editing an MCP server
#[derive(Deserialize, Serialize)]
pub struct EditMcpToolArgs {
    /// Name of the MCP server to edit
    pub name: String,
    /// New HTTP URL for the server endpoint (optional, keeps existing if not provided)
    #[serde(default)]
    pub url: Option<String>,
    /// New Bearer token for authentication (optional).
    /// - Omit or pass `null` to leave the existing key unchanged.
    /// - Pass `"****"` to explicitly keep the existing key (safe sentinel for masked display).
    /// - Pass `""` (empty string) to remove authentication entirely.
    /// - Pass the new token value to update it.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Output from the edit_mcp tool
#[derive(Debug, Serialize)]
pub struct EditMcpToolOutput {
    pub success: bool,
    pub message: String,
    pub server_name: String,
}

/// Tool that allows the LLM to edit existing MCP server configurations
#[derive(Clone)]
pub struct EditMcpTool {
    repository: Arc<dyn McpRepository>,
    /// Notifies the UI after a successful save. None in tests.
    update_sender: Option<tokio::sync::mpsc::Sender<Vec<McpServerConfig>>>,
}

impl EditMcpTool {
    /// Test constructor: no live services injected.
    pub fn new(repository: Arc<dyn McpRepository>) -> Self {
        Self {
            repository,
            update_sender: None,
        }
    }

    /// Production constructor: inject real sender and service.
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

/// Validate the edit arguments, returning an error message if invalid.
fn validate_edit_args(args: &EditMcpToolArgs) -> Result<(), String> {
    let name = args.name.trim();

    if name.is_empty() {
        return Err("Server name cannot be empty".to_string());
    }

    // Validate URL if provided
    if let Some(ref url) = args.url {
        let url = url.trim();
        if url.is_empty() {
            return Err("URL cannot be empty".to_string());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(
                "URL must start with http:// or https:// (e.g. \"http://localhost:3000/mcp\")"
                    .to_string(),
            );
        }
    }

    // Must provide at least one field to update
    if args.url.is_none() && args.api_key.is_none() {
        return Err("At least one field (url, api_key) must be provided to edit".to_string());
    }

    Ok(())
}

impl Tool for EditMcpTool {
    const NAME: &'static str = "edit_mcp_service";
    type Error = EditMcpToolError;
    type Args = EditMcpToolArgs;
    type Output = EditMcpToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "edit_mcp_service".to_string(),
            description: "Edit an existing MCP (Model Context Protocol) server configuration. \
                         This updates the server's URL and/or API key. Only the fields you \
                         provide will be changed; omitted fields keep their current values. \
                         \n\n\
                         Use this when the user wants to update an MCP server's URL or \
                         authentication token. \
                         \n\n\
                         For the api_key field:\n\
                         - Omit or pass null to leave the existing key unchanged.\n\
                         - Pass '****' to explicitly keep the existing key.\n\
                         - Pass '' (empty string) to remove authentication.\n\
                         - Pass the new token value to update it.\n\
                         \n\
                         Note: enabling or disabling a server can only be done by the user via \
                         Settings → Execution → MCP Servers."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the MCP server to edit (e.g., 'my-tools', 'github')"
                    },
                    "url": {
                        "type": "string",
                        "description": "New HTTP URL for the server endpoint (e.g., 'http://localhost:3000/mcp'). Omit to keep current value."
                    },
                    "api_key": {
                        "type": "string",
                        "description": "Bearer token for authentication. Omit/null = no change. '****' = keep existing. '' = remove auth. New value = update token."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate before acquiring the lock
        validate_edit_args(&args).map_err(EditMcpToolError::ValidationError)?;

        let name = args.name.trim().to_string();
        let server_name = name.clone();

        // Acquire shared write lock: makes load → find → update → save atomic.
        let _guard = MCP_WRITE_LOCK.lock().await;

        // Load existing servers
        let mut servers = self.repository.load_all().await.map_err(|e| {
            EditMcpToolError::RepositoryError(format!("Failed to load servers: {}", e))
        })?;

        // Find the server to edit
        let server = servers.iter_mut().find(|s| s.name == name);
        let Some(server) = server else {
            return Ok(EditMcpToolOutput {
                success: false,
                message: format!(
                    "No MCP server named '{}' was found. Use list_mcp_services to see available servers.",
                    name
                ),
                server_name,
            });
        };

        let mut changes = Vec::new();

        // Apply updates (only fields that are Some)
        if let Some(url) = args.url {
            server.url = url;
            changes.push("url");
        }

        // Handle api_key changes with sentinel support:
        // - Some("****") → keep existing
        // - Some("") → clear authentication
        // - Some(token) → set new token
        // - None → no change
        if let Some(ref key_value) = args.api_key {
            if key_value == MASKED_API_KEY_SENTINEL {
                // Sentinel: keep existing key unchanged
            } else if key_value.is_empty() {
                // Empty string: remove authentication
                server.api_key = None;
                changes.push("api_key (removed)");
            } else {
                // New token value
                server.api_key = Some(key_value.clone());
                changes.push("api_key");
            }
        }

        tracing::info!(
            server_name = %name,
            changes = ?changes,
            "Editing MCP server configuration"
        );

        // Save to disk inside the lock
        self.repository
            .save_all(servers.clone())
            .await
            .map_err(|e| {
                EditMcpToolError::RepositoryError(format!("Failed to save servers: {}", e))
            })?;

        // Release lock before best-effort notification.
        drop(_guard);

        // Notify the UI to refresh
        if let Some(ref tx) = self.update_sender
            && let Err(e) = tx.send(servers).await
        {
            tracing::warn!(error = ?e, "Failed to send MCP update notification");
        }

        Ok(EditMcpToolOutput {
            success: true,
            message: format!(
                "MCP server '{}' has been updated ({}).",
                server_name,
                changes.join(", ")
            ),
            server_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_helpers::MockMcpRepository;

    fn test_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: true,
            is_module: false,
        }
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_empty_name() {
        let args = EditMcpToolArgs {
            name: "".to_string(),
            url: Some("http://localhost:3000/mcp".to_string()),
            api_key: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(validate_edit_args(&args).unwrap_err().contains("name"));
    }

    #[test]
    fn test_validate_no_fields_to_update() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            url: None,
            api_key: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(
            validate_edit_args(&args)
                .unwrap_err()
                .contains("At least one field")
        );
    }

    #[test]
    fn test_validate_empty_url() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            url: Some("".to_string()),
            api_key: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(
            validate_edit_args(&args)
                .unwrap_err()
                .contains("URL cannot be empty")
        );
    }

    #[test]
    fn test_validate_url_without_scheme() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            url: Some("localhost:3000/mcp".to_string()),
            api_key: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(validate_edit_args(&args).unwrap_err().contains("http://"));
    }

    #[test]
    fn test_validate_valid_url() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            url: Some("http://localhost:9000/mcp".to_string()),
            api_key: None,
        };
        assert!(validate_edit_args(&args).is_ok());
    }

    // --- Tool::call integration tests ---

    #[tokio::test]
    async fn test_edit_url() {
        let servers = vec![test_server("my-server")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "my-server".to_string(),
                url: Some("http://localhost:9000/mcp".to_string()),
                api_key: None,
            })
            .await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.success);
        assert!(output.message.contains("url"));

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].url, "http://localhost:9000/mcp");
        assert!(saved[0].enabled);
    }

    #[tokio::test]
    async fn test_edit_nonexistent_server() {
        let servers = vec![test_server("existing")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "nonexistent".to_string(),
                url: Some("http://localhost:9000/mcp".to_string()),
                api_key: None,
            })
            .await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.success);
        assert!(output.message.contains("No MCP server named"));

        assert!(repo.get_last_saved().is_none());
    }

    #[tokio::test]
    async fn test_edit_preserves_other_servers() {
        let servers = vec![
            test_server("server-a"),
            test_server("server-b"),
            test_server("server-c"),
        ];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "server-b".to_string(),
                url: Some("http://localhost:9000/mcp".to_string()),
                api_key: None,
            })
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved.len(), 3);
        assert_eq!(saved[0].url, "http://localhost:3000/mcp"); // unchanged
        assert_eq!(saved[1].url, "http://localhost:9000/mcp"); // updated
        assert_eq!(saved[2].url, "http://localhost:3000/mcp"); // unchanged
    }

    #[tokio::test]
    async fn test_edit_load_error() {
        let repo = Arc::new(MockMcpRepository::with_load_error("disk read failure"));
        let tool = EditMcpTool::new(repo);

        let result = tool
            .call(EditMcpToolArgs {
                name: "any".to_string(),
                url: Some("http://localhost:9000/mcp".to_string()),
                api_key: None,
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EditMcpToolError::RepositoryError(_)
        ));
    }

    #[tokio::test]
    async fn test_edit_save_error() {
        let servers = vec![test_server("target")];
        let repo = Arc::new(MockMcpRepository::with_save_error(
            servers,
            "disk write failure",
        ));
        let tool = EditMcpTool::new(repo);

        let result = tool
            .call(EditMcpToolArgs {
                name: "target".to_string(),
                url: Some("http://localhost:9000/mcp".to_string()),
                api_key: None,
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EditMcpToolError::RepositoryError(_)
        ));
    }

    // --- Tool definition tests ---

    #[tokio::test]
    async fn test_definition_has_correct_name() {
        let repo = Arc::new(MockMcpRepository::with_servers(vec![]));
        let tool = EditMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "edit_mcp_service");
    }

    #[tokio::test]
    async fn test_definition_has_required_fields() {
        let repo = Arc::new(MockMcpRepository::with_servers(vec![]));
        let tool = EditMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let required = def.parameters["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"name"));
    }

    #[tokio::test]
    async fn test_definition_has_all_properties() {
        let repo = Arc::new(MockMcpRepository::with_servers(vec![]));
        let tool = EditMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("url"));
        assert!(!props.contains_key("enabled")); // user-only control, not in schema
    }

    // --- Serde tests ---

    #[test]
    fn test_args_deserialize_minimal() {
        let json = r#"{"name": "test"}"#;
        let args: EditMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert!(args.url.is_none());
    }

    #[test]
    fn test_args_deserialize_with_url() {
        let json = r#"{"name": "test", "url": "http://localhost:9000/mcp"}"#;
        let args: EditMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert_eq!(args.url.unwrap(), "http://localhost:9000/mcp");
    }

    #[test]
    fn test_output_serialization() {
        let output = EditMcpToolOutput {
            success: true,
            message: "Updated".to_string(),
            server_name: "test".to_string(),
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"server_name\":\"test\""));
    }

    // --- Error display tests ---

    #[test]
    fn test_validation_error_display() {
        let err = EditMcpToolError::ValidationError("bad input".to_string());
        assert_eq!(err.to_string(), "Validation error: bad input");
    }

    #[test]
    fn test_repository_error_display() {
        let err = EditMcpToolError::RepositoryError("disk full".to_string());
        assert_eq!(err.to_string(), "Repository error: disk full");
    }

    #[test]
    fn test_tool_name_constant() {
        assert_eq!(EditMcpTool::NAME, "edit_mcp_service");
    }
}
