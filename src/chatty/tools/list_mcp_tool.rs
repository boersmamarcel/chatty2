use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::settings::repositories::McpRepository;

/// Arguments for listing MCP servers (no arguments needed)
#[derive(Deserialize, Serialize)]
pub struct ListMcpToolArgs {}

/// Summary of a single configured MCP server, safe for display to the LLM.
///
/// Sensitive env var values are masked with `"****"` via `McpServerConfig::masked_env()`.
#[derive(Debug, Serialize)]
pub struct McpServerSummary {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Env vars with sensitive values masked (KEY, TOKEN, SECRET, etc. → "****")
    pub env: std::collections::HashMap<String, String>,
    pub enabled: bool,
}

/// Output from the list_mcp_services tool
#[derive(Debug, Serialize)]
pub struct ListMcpToolOutput {
    pub servers: Vec<McpServerSummary>,
    pub total: usize,
    pub note: String,
}

/// Error type for list_mcp tool
#[derive(Debug, thiserror::Error)]
pub enum ListMcpToolError {
    #[error("Repository error: {0}")]
    RepositoryError(String),
}

/// Tool that lists all configured MCP servers (with masked sensitive env vars).
///
/// This gives the LLM visibility into what MCP servers are already configured,
/// so it can edit or delete the right ones instead of adding duplicates.
#[derive(Clone)]
pub struct ListMcpTool {
    repository: Arc<dyn McpRepository>,
}

impl ListMcpTool {
    pub fn new(repository: Arc<dyn McpRepository>) -> Self {
        Self { repository }
    }
}

impl Tool for ListMcpTool {
    const NAME: &'static str = "list_mcp_services";
    type Error = ListMcpToolError;
    type Args = ListMcpToolArgs;
    type Output = ListMcpToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_mcp_services".to_string(),
            description: "List all configured MCP (Model Context Protocol) server configurations. \
                         Returns each server's name, command, args, enabled state, and env vars \
                         (with sensitive values like API keys masked as '****'). \
                         \n\n\
                         Call this BEFORE editing or deleting an MCP server to confirm the exact \
                         server name and current configuration. This prevents accidentally \
                         adding a duplicate instead of modifying an existing server."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let servers = self.repository.load_all().await.map_err(|e| {
            ListMcpToolError::RepositoryError(format!("Failed to load servers: {}", e))
        })?;

        tracing::info!(server_count = servers.len(), "list_mcp_services called");

        let summaries: Vec<McpServerSummary> = servers
            .iter()
            .map(|s| McpServerSummary {
                name: s.name.clone(),
                command: s.command.clone(),
                args: s.args.clone(),
                env: s.masked_env(),
                enabled: s.enabled,
            })
            .collect();

        let total = summaries.len();
        let note = if total == 0 {
            "No MCP servers are configured. Use add_mcp_service to add one.".to_string()
        } else {
            "Sensitive env var values (API keys, tokens, etc.) are shown as '****'. \
             When editing, send back '****' for any key you want to keep unchanged."
                .to_string()
        };

        Ok(ListMcpToolOutput {
            servers: summaries,
            total,
            note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::tools::test_helpers::MockMcpRepository;
    use crate::settings::models::mcp_store::McpServerConfig;
    use std::collections::HashMap;

    fn make_server(name: &str, api_key: &str) -> McpServerConfig {
        let mut env = HashMap::new();
        env.insert("TAVILY_API_KEY".to_string(), api_key.to_string());
        env.insert("HOST".to_string(), "localhost".to_string());
        McpServerConfig {
            name: name.to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@tavily/mcp".to_string()],
            env,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn test_list_empty_repo() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.total, 0);
        assert!(output.servers.is_empty());
        assert!(output.note.contains("No MCP servers"));
    }

    #[tokio::test]
    async fn test_list_masks_sensitive_env_vars() {
        let server = make_server("tavily", "tvly-real-secret");
        let repo = Arc::new(MockMcpRepository::with_servers(vec![server]));
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.total, 1);

        let s = &output.servers[0];
        assert_eq!(s.name, "tavily");
        // Sensitive key must be masked
        assert_eq!(s.env.get("TAVILY_API_KEY").unwrap(), "****");
        // Non-sensitive key must be visible
        assert_eq!(s.env.get("HOST").unwrap(), "localhost");
    }

    #[tokio::test]
    async fn test_list_multiple_servers() {
        let servers = vec![
            make_server("server-a", "key-a"),
            make_server("server-b", "key-b"),
        ];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.total, 2);
        assert_eq!(output.servers[0].name, "server-a");
        assert_eq!(output.servers[1].name, "server-b");
        // Both real API keys must be hidden
        assert_eq!(output.servers[0].env.get("TAVILY_API_KEY").unwrap(), "****");
        assert_eq!(output.servers[1].env.get("TAVILY_API_KEY").unwrap(), "****");
    }

    #[tokio::test]
    async fn test_list_returns_correct_fields() {
        let server = make_server("my-server", "real-key");
        let repo = Arc::new(MockMcpRepository::with_servers(vec![server]));
        let tool = ListMcpTool::new(repo);

        let output = tool.call(ListMcpToolArgs {}).await.unwrap();
        let s = &output.servers[0];
        assert_eq!(s.command, "npx");
        assert_eq!(s.args, vec!["-y", "@tavily/mcp"]);
        assert!(s.enabled);
    }

    #[tokio::test]
    async fn test_list_load_error_propagates() {
        let repo = Arc::new(MockMcpRepository::with_load_error("disk failure"));
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to load servers")
        );
    }

    #[tokio::test]
    async fn test_definition_has_correct_name() {
        let repo = Arc::new(MockMcpRepository::new());
        let tool = ListMcpTool::new(repo);
        let def = tool.definition("".to_string()).await;
        assert_eq!(def.name, "list_mcp_services");
    }

    #[test]
    fn test_tool_name_constant() {
        assert_eq!(ListMcpTool::NAME, "list_mcp_services");
    }
}
