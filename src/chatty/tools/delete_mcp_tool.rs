use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::settings::models::mcp_store::{MCP_WRITE_LOCK, McpServerConfig};
use crate::settings::repositories::McpRepository;

/// Error type for delete_mcp tool
#[derive(Debug, thiserror::Error)]
pub enum DeleteMcpToolError {
    #[error("Validation error: {0}")]
    ValidationError(String),
    #[error("Repository error: {0}")]
    RepositoryError(String),
}

/// Arguments for deleting an MCP server
#[derive(Deserialize, Serialize)]
pub struct DeleteMcpToolArgs {
    /// Name of the MCP server to delete
    pub name: String,
}

/// Output from the delete_mcp tool
#[derive(Debug, Serialize)]
pub struct DeleteMcpToolOutput {
    pub success: bool,
    pub message: String,
    pub server_name: String,
}

/// Tool that allows the LLM to delete MCP server configurations
#[derive(Clone)]
pub struct DeleteMcpTool {
    repository: Arc<dyn McpRepository>,
    /// Notifies the UI after a successful save. None in tests.
    update_sender: Option<tokio::sync::mpsc::Sender<Vec<McpServerConfig>>>,
    /// Stops the server immediately after removing. None in tests.
    mcp_service: Option<crate::chatty::services::McpService>,
}

impl DeleteMcpTool {
    /// Test constructor: no live services injected.
    pub fn new(repository: Arc<dyn McpRepository>) -> Self {
        Self {
            repository,
            update_sender: None,
            mcp_service: None,
        }
    }

    /// Production constructor: inject real sender and service.
    pub fn new_with_services(
        repository: Arc<dyn McpRepository>,
        update_sender: tokio::sync::mpsc::Sender<Vec<McpServerConfig>>,
        mcp_service: crate::chatty::services::McpService,
    ) -> Self {
        Self {
            repository,
            update_sender: Some(update_sender),
            mcp_service: Some(mcp_service),
        }
    }
}

impl Tool for DeleteMcpTool {
    const NAME: &'static str = "delete_mcp_service";
    type Error = DeleteMcpToolError;
    type Args = DeleteMcpToolArgs;
    type Output = DeleteMcpToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "delete_mcp_service".to_string(),
            description: "Delete an existing MCP (Model Context Protocol) server configuration. \
                         This removes the server and stops it if currently running. \
                         \n\n\
                         Use this when the user wants to remove an MCP service they no longer need. \
                         The server will be stopped immediately if it is running."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the MCP server to delete (e.g., 'tavily-search', 'github')"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let name = args.name.trim().to_string();

        // Validate name is not empty
        if name.is_empty() {
            return Err(DeleteMcpToolError::ValidationError(
                "Server name cannot be empty".to_string(),
            ));
        }

        let server_name = name.clone();

        // Acquire shared write lock: makes load → check → remove → save atomic
        // across all MCP tools (add, delete, edit).
        let _guard = MCP_WRITE_LOCK.lock().await;

        // Load existing servers
        let mut servers = self.repository.load_all().await.map_err(|e| {
            DeleteMcpToolError::RepositoryError(format!("Failed to load servers: {}", e))
        })?;

        // Find the server to delete
        let original_len = servers.len();
        servers.retain(|s| s.name != name);

        if servers.len() == original_len {
            // Server was not found
            return Ok(DeleteMcpToolOutput {
                success: false,
                message: format!(
                    "No MCP server named '{}' was found. Use list_tools to see available servers.",
                    name
                ),
                server_name,
            });
        }

        tracing::info!(
            server_name = %name,
            "Deleting MCP server configuration"
        );

        // Save to disk inside the lock
        self.repository
            .save_all(servers.clone())
            .await
            .map_err(|e| {
                DeleteMcpToolError::RepositoryError(format!("Failed to save servers: {}", e))
            })?;

        // Release lock before best-effort notification and server stop.
        drop(_guard);

        // Notify the UI to refresh
        if let Some(ref tx) = self.update_sender
            && let Err(e) = tx.send(servers).await
        {
            tracing::warn!(error = ?e, "Failed to send MCP update notification");
        }

        // Stop the server process
        if let Some(ref svc) = self.mcp_service {
            if let Err(e) = svc.stop_server(&name).await {
                tracing::warn!(server = %name, error = ?e, "Failed to stop MCP server during deletion");
            }
        }

        Ok(DeleteMcpToolOutput {
            success: true,
            message: format!("MCP server '{}' has been deleted and stopped.", server_name),
            server_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::repositories::mcp_repository::BoxFuture;
    use crate::settings::repositories::provider_repository::{RepositoryError, RepositoryResult};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // --- Mock repository for testing ---

    struct MockMcpRepository {
        servers: Mutex<Vec<McpServerConfig>>,
        load_error: Mutex<Option<String>>,
        save_error: Mutex<Option<String>>,
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

    fn test_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            env: HashMap::new(),
            enabled: true,
        }
    }

    // --- Validation tests ---

    #[tokio::test]
    async fn test_delete_empty_name_returns_err() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = DeleteMcpTool::new(repo);

        let args = DeleteMcpToolArgs {
            name: "".to_string(),
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DeleteMcpToolError::ValidationError(_)
        ));
    }

    #[tokio::test]
    async fn test_delete_whitespace_name_returns_err() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = DeleteMcpTool::new(repo);

        let args = DeleteMcpToolArgs {
            name: "   ".to_string(),
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }

    // --- Tool::call integration tests ---

    #[tokio::test]
    async fn test_delete_existing_server() {
        let servers = vec![
            test_server("server-a"),
            test_server("server-b"),
            test_server("server-c"),
        ];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = DeleteMcpTool::new(repo.clone());

        let result = tool
            .call(DeleteMcpToolArgs {
                name: "server-b".to_string(),
            })
            .await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.success);
        assert_eq!(output.server_name, "server-b");
        assert!(output.message.contains("deleted"));

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].name, "server-a");
        assert_eq!(saved[1].name, "server-c");
    }

    #[tokio::test]
    async fn test_delete_nonexistent_server_returns_failure() {
        let servers = vec![test_server("existing")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = DeleteMcpTool::new(repo.clone());

        let result = tool
            .call(DeleteMcpToolArgs {
                name: "nonexistent".to_string(),
            })
            .await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.success);
        assert!(output.message.contains("No MCP server named"));

        // Nothing should have been saved
        assert!(repo.get_last_saved().is_none());
    }

    #[tokio::test]
    async fn test_delete_last_server_leaves_empty_list() {
        let servers = vec![test_server("only-one")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = DeleteMcpTool::new(repo.clone());

        let result = tool
            .call(DeleteMcpToolArgs {
                name: "only-one".to_string(),
            })
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert!(saved.is_empty());
    }

    #[tokio::test]
    async fn test_delete_load_error() {
        let repo = Arc::new(MockMcpRepository::with_load_error("disk read failure"));
        let tool = DeleteMcpTool::new(repo);

        let result = tool
            .call(DeleteMcpToolArgs {
                name: "any".to_string(),
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DeleteMcpToolError::RepositoryError(_)
        ));
    }

    #[tokio::test]
    async fn test_delete_save_error() {
        let servers = vec![test_server("target")];
        let repo = Arc::new(MockMcpRepository::with_save_error(
            servers,
            "disk write failure",
        ));
        let tool = DeleteMcpTool::new(repo);

        let result = tool
            .call(DeleteMcpToolArgs {
                name: "target".to_string(),
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DeleteMcpToolError::RepositoryError(_)
        ));
    }

    // --- Tool definition tests ---

    #[tokio::test]
    async fn test_definition_has_correct_name() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = DeleteMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "delete_mcp_service");
    }

    #[tokio::test]
    async fn test_definition_has_required_fields() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = DeleteMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let required = def.parameters["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"name"));
    }

    // --- Serde tests ---

    #[test]
    fn test_args_deserialize() {
        let json = r#"{"name": "test-server"}"#;
        let args: DeleteMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test-server");
    }

    #[test]
    fn test_args_deserialize_missing_name_fails() {
        let json = r#"{}"#;
        let result: Result<DeleteMcpToolArgs, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_output_serialization() {
        let output = DeleteMcpToolOutput {
            success: true,
            message: "Deleted".to_string(),
            server_name: "test".to_string(),
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"server_name\":\"test\""));
    }

    // --- Error display tests ---

    #[test]
    fn test_validation_error_display() {
        let err = DeleteMcpToolError::ValidationError("bad name".to_string());
        assert_eq!(err.to_string(), "Validation error: bad name");
    }

    #[test]
    fn test_repository_error_display() {
        let err = DeleteMcpToolError::RepositoryError("disk full".to_string());
        assert_eq!(err.to_string(), "Repository error: disk full");
    }

    #[test]
    fn test_tool_name_constant() {
        assert_eq!(DeleteMcpTool::NAME, "delete_mcp_service");
    }
}
