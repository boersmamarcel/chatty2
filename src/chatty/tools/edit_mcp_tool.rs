use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::repositories::McpRepository;

lazy_static::lazy_static! {
    /// Serialises concurrent edit_mcp_service calls so the
    /// load → find → update → save sequence is atomic.
    static ref WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

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
    /// New command to execute (optional, keeps existing if not provided)
    #[serde(default)]
    pub command: Option<String>,
    /// New command-line arguments (optional, keeps existing if not provided)
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// New environment variables (optional, keeps existing if not provided).
    /// When provided, fully replaces the existing env vars.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// Enable or disable the server (optional, keeps existing if not provided)
    #[serde(default)]
    pub enabled: Option<bool>,
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
    /// Restarts the server after editing. None in tests.
    mcp_service: Option<crate::chatty::services::McpService>,
}

impl EditMcpTool {
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

/// Validate the edit arguments, returning an error message if invalid.
fn validate_edit_args(args: &EditMcpToolArgs) -> Result<(), String> {
    let name = args.name.trim();

    if name.is_empty() {
        return Err("Server name cannot be empty".to_string());
    }

    // Validate command if provided
    if let Some(ref cmd) = args.command {
        if cmd.trim().is_empty() {
            return Err("Command cannot be empty".to_string());
        }
    }

    // Validate env var keys if provided
    if let Some(ref env) = args.env {
        for key in env.keys() {
            if key.trim().is_empty() {
                return Err("Environment variable keys cannot be empty".to_string());
            }
        }
    }

    // Must provide at least one field to update
    if args.command.is_none() && args.args.is_none() && args.env.is_none() && args.enabled.is_none()
    {
        return Err(
            "At least one field (command, args, env, enabled) must be provided to edit".to_string(),
        );
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
                         This updates the server's command, arguments, environment variables, \
                         or enabled state. Only the fields you provide will be changed; \
                         omitted fields keep their current values. \
                         \n\n\
                         Use this when the user wants to update an MCP server's configuration, \
                         such as changing environment variables (e.g., API keys), updating the \
                         command or arguments, or enabling/disabling a server. \
                         The server will be restarted automatically if it was running."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the MCP server to edit (e.g., 'tavily-search', 'github')"
                    },
                    "command": {
                        "type": "string",
                        "description": "New command to execute (e.g., 'npx', 'uvx'). Omit to keep current value."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "New command-line arguments. Omit to keep current value."
                    },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "New environment variables. When provided, fully replaces existing env vars. Omit to keep current value."
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "Enable (true) or disable (false) the server. Omit to keep current value."
                    }
                },
                "required": ["name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate before acquiring the lock
        validate_edit_args(&args).map_err(EditMcpToolError::ValidationError)?;

        let server_name = args.name.clone();

        // Acquire write lock: makes load → find → update → save atomic.
        let _guard = WRITE_LOCK.lock().await;

        // Load existing servers
        let mut servers = self.repository.load_all().await.map_err(|e| {
            EditMcpToolError::RepositoryError(format!("Failed to load servers: {}", e))
        })?;

        // Find the server to edit
        let server = servers.iter_mut().find(|s| s.name == args.name);
        let Some(server) = server else {
            return Ok(EditMcpToolOutput {
                success: false,
                message: format!(
                    "No MCP server named '{}' was found. Use list_tools to see available servers.",
                    args.name
                ),
                server_name,
            });
        };

        // Track what changed for the log message
        let mut changes = Vec::new();

        // Apply updates (only fields that are Some)
        if let Some(command) = args.command {
            server.command = command;
            changes.push("command");
        }
        if let Some(new_args) = args.args {
            server.args = new_args;
            changes.push("args");
        }
        if let Some(env) = args.env {
            server.env = env;
            changes.push("env");
        }
        if let Some(enabled) = args.enabled {
            server.enabled = enabled;
            changes.push("enabled");
        }

        let updated_server = server.clone();
        let server_enabled = updated_server.enabled;

        tracing::info!(
            server_name = %args.name,
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

        // Release lock before best-effort notification and server restart.
        drop(_guard);

        // Notify the UI to refresh
        if let Some(ref tx) = self.update_sender
            && let Err(e) = tx.send(servers).await
        {
            tracing::warn!(error = ?e, "Failed to send MCP update notification");
        }

        // Restart the server if it was enabled (stop then start)
        if let Some(ref svc) = self.mcp_service {
            // Always stop the old instance
            if let Err(e) = svc.stop_server(&args.name).await {
                tracing::warn!(server = %args.name, error = ?e, "Failed to stop MCP server for restart");
            }
            // Start new instance if enabled
            if server_enabled {
                if let Err(e) = svc.start_server(updated_server).await {
                    tracing::warn!(server = %args.name, error = ?e, "MCP server edited but failed to restart");
                    return Ok(EditMcpToolOutput {
                        success: true,
                        message: format!(
                            "MCP server '{}' configuration updated ({}) but failed to restart ({}). \
                             It will be available after restarting the application.",
                            server_name,
                            changes.join(", "),
                            e
                        ),
                        server_name,
                    });
                }
            }
        }

        let restart_msg = if server_enabled {
            " and restarted. Start a new conversation to use the updated tools."
        } else {
            " (server is disabled)."
        };

        Ok(EditMcpToolOutput {
            success: true,
            message: format!(
                "MCP server '{}' has been updated ({}){}",
                server_name,
                changes.join(", "),
                restart_msg
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

    struct MockMcpRepository {
        servers: Mutex<Vec<McpServerConfig>>,
        load_error: Mutex<Option<String>>,
        save_error: Mutex<Option<String>>,
        last_saved: Mutex<Option<Vec<McpServerConfig>>>,
    }

    impl MockMcpRepository {
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

    fn test_server_with_env(name: &str, env: HashMap<String, String>) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@test/server".to_string()],
            env,
            enabled: true,
        }
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_empty_name() {
        let args = EditMcpToolArgs {
            name: "".to_string(),
            command: Some("npx".to_string()),
            args: None,
            env: None,
            enabled: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(validate_edit_args(&args).unwrap_err().contains("name"));
    }

    #[test]
    fn test_validate_no_fields_to_update() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            command: None,
            args: None,
            env: None,
            enabled: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(
            validate_edit_args(&args)
                .unwrap_err()
                .contains("At least one field")
        );
    }

    #[test]
    fn test_validate_empty_command() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            command: Some("".to_string()),
            args: None,
            env: None,
            enabled: None,
        };
        assert!(validate_edit_args(&args).is_err());
        assert!(
            validate_edit_args(&args)
                .unwrap_err()
                .contains("Command cannot be empty")
        );
    }

    #[test]
    fn test_validate_empty_env_key() {
        let mut env = HashMap::new();
        env.insert("".to_string(), "value".to_string());
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            command: None,
            args: None,
            env: Some(env),
            enabled: None,
        };
        assert!(validate_edit_args(&args).is_err());
    }

    #[test]
    fn test_validate_valid_command_only() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            command: Some("uvx".to_string()),
            args: None,
            env: None,
            enabled: None,
        };
        assert!(validate_edit_args(&args).is_ok());
    }

    #[test]
    fn test_validate_valid_enabled_only() {
        let args = EditMcpToolArgs {
            name: "server".to_string(),
            command: None,
            args: None,
            env: None,
            enabled: Some(false),
        };
        assert!(validate_edit_args(&args).is_ok());
    }

    // --- Tool::call integration tests ---

    #[tokio::test]
    async fn test_edit_command() {
        let servers = vec![test_server("my-server")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "my-server".to_string(),
                command: Some("uvx".to_string()),
                args: None,
                env: None,
                enabled: None,
            })
            .await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.success);
        assert!(output.message.contains("command"));

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].command, "uvx");
        // Other fields unchanged
        assert_eq!(saved[0].args, vec!["test"]);
        assert!(saved[0].enabled);
    }

    #[tokio::test]
    async fn test_edit_args() {
        let servers = vec![test_server("my-server")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "my-server".to_string(),
                command: None,
                args: Some(vec!["new-arg".to_string()]),
                env: None,
                enabled: None,
            })
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].args, vec!["new-arg"]);
        assert_eq!(saved[0].command, "echo"); // unchanged
    }

    #[tokio::test]
    async fn test_edit_env() {
        let mut original_env = HashMap::new();
        original_env.insert("OLD_KEY".to_string(), "old-val".to_string());
        let servers = vec![test_server_with_env("my-server", original_env)];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let mut new_env = HashMap::new();
        new_env.insert("NEW_KEY".to_string(), "new-val".to_string());

        let result = tool
            .call(EditMcpToolArgs {
                name: "my-server".to_string(),
                command: None,
                args: None,
                env: Some(new_env),
                enabled: None,
            })
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].env.len(), 1);
        assert_eq!(saved[0].env.get("NEW_KEY").unwrap(), "new-val");
        assert!(!saved[0].env.contains_key("OLD_KEY")); // fully replaced
    }

    #[tokio::test]
    async fn test_edit_enabled() {
        let servers = vec![test_server("my-server")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "my-server".to_string(),
                command: None,
                args: None,
                env: None,
                enabled: Some(false),
            })
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert!(!saved[0].enabled);
    }

    #[tokio::test]
    async fn test_edit_multiple_fields() {
        let servers = vec![test_server("my-server")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let mut env = HashMap::new();
        env.insert("KEY".to_string(), "val".to_string());

        let result = tool
            .call(EditMcpToolArgs {
                name: "my-server".to_string(),
                command: Some("docker".to_string()),
                args: Some(vec!["run".to_string(), "img".to_string()]),
                env: Some(env),
                enabled: None,
            })
            .await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.success);
        assert!(output.message.contains("command"));
        assert!(output.message.contains("args"));
        assert!(output.message.contains("env"));

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved[0].command, "docker");
        assert_eq!(saved[0].args, vec!["run", "img"]);
        assert_eq!(saved[0].env.get("KEY").unwrap(), "val");
    }

    #[tokio::test]
    async fn test_edit_nonexistent_server() {
        let servers = vec![test_server("existing")];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = EditMcpTool::new(repo.clone());

        let result = tool
            .call(EditMcpToolArgs {
                name: "nonexistent".to_string(),
                command: Some("npx".to_string()),
                args: None,
                env: None,
                enabled: None,
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
                command: Some("new-cmd".to_string()),
                args: None,
                env: None,
                enabled: None,
            })
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        let saved = repo.get_last_saved().unwrap();
        assert_eq!(saved.len(), 3);
        assert_eq!(saved[0].command, "echo"); // unchanged
        assert_eq!(saved[1].command, "new-cmd"); // updated
        assert_eq!(saved[2].command, "echo"); // unchanged
    }

    #[tokio::test]
    async fn test_edit_load_error() {
        let repo = Arc::new(MockMcpRepository::with_load_error("disk read failure"));
        let tool = EditMcpTool::new(repo);

        let result = tool
            .call(EditMcpToolArgs {
                name: "any".to_string(),
                command: Some("npx".to_string()),
                args: None,
                env: None,
                enabled: None,
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
                command: Some("new".to_string()),
                args: None,
                env: None,
                enabled: None,
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
        // Optional fields should not be required
        assert!(!required_names.contains(&"command"));
        assert!(!required_names.contains(&"args"));
        assert!(!required_names.contains(&"env"));
        assert!(!required_names.contains(&"enabled"));
    }

    #[tokio::test]
    async fn test_definition_has_all_properties() {
        let repo = Arc::new(MockMcpRepository::with_servers(vec![]));
        let tool = EditMcpTool::new(repo);

        let def = tool.definition("test".to_string()).await;
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("command"));
        assert!(props.contains_key("args"));
        assert!(props.contains_key("env"));
        assert!(props.contains_key("enabled"));
    }

    // --- Serde tests ---

    #[test]
    fn test_args_deserialize_minimal() {
        let json = r#"{"name": "test", "command": "npx"}"#;
        let args: EditMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert_eq!(args.command.unwrap(), "npx");
        assert!(args.args.is_none());
        assert!(args.env.is_none());
        assert!(args.enabled.is_none());
    }

    #[test]
    fn test_args_deserialize_name_only() {
        // This is valid JSON but will fail validation (no fields to update)
        let json = r#"{"name": "test"}"#;
        let args: EditMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert!(args.command.is_none());
    }

    #[test]
    fn test_args_deserialize_full() {
        let json = r#"{
            "name": "server",
            "command": "uvx",
            "args": ["pkg"],
            "env": {"KEY": "val"},
            "enabled": false
        }"#;
        let args: EditMcpToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "server");
        assert_eq!(args.command.unwrap(), "uvx");
        assert_eq!(args.args.unwrap(), vec!["pkg"]);
        assert_eq!(args.env.unwrap().get("KEY").unwrap(), "val");
        assert_eq!(args.enabled.unwrap(), false);
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
