use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::settings::repositories::McpRepository;
use crate::tools::ToolError;

/// Arguments for listing MCP servers (no arguments needed)
#[derive(Deserialize, Serialize)]
pub struct ListMcpToolArgs {}

/// Summary of a single configured MCP server, safe for display to the LLM.
#[derive(Debug, Serialize)]
pub struct McpServerSummary {
    pub name: String,
    pub url: String,
    /// `true` if an API key is configured (value is never exposed).
    pub has_api_key: bool,
    pub enabled: bool,
}

/// Output from the list_mcp_services tool
#[derive(Debug, Serialize)]
pub struct ListMcpToolOutput {
    pub servers: Vec<McpServerSummary>,
    pub total: usize,
    pub note: String,
}

/// Tool that lists all configured MCP servers.
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
    type Error = ToolError;
    type Args = ListMcpToolArgs;
    type Output = ListMcpToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_mcp_services".to_string(),
            description: "List all configured MCP (Model Context Protocol) server configurations. \
                         Returns each server's name, URL, and enabled state. \
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
        let servers =
            self.repository.load_all().await.map_err(|e| {
                ToolError::OperationFailed(format!("Failed to load servers: {}", e))
            })?;

        tracing::info!(server_count = servers.len(), "list_mcp_services called");

        let summaries: Vec<McpServerSummary> = servers
            .iter()
            .map(|s| McpServerSummary {
                name: s.name.clone(),
                url: s.url.clone(),
                has_api_key: s.has_api_key(),
                enabled: s.enabled,
            })
            .collect();

        let total = summaries.len();
        let note = if total == 0 {
            "No MCP servers are configured. Ask the user to add one in Settings → Extensions."
                .to_string()
        } else {
            "Servers must already be running at the configured URL before they can be enabled."
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
    use crate::settings::models::mcp_store::McpServerConfig;
    use crate::tools::test_helpers::MockMcpRepository;

    fn make_server(name: &str, url: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            url: url.to_string(),
            api_key: None,
            enabled: true,
            is_module: false,
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
    async fn test_list_returns_url() {
        let server = make_server("my-server", "http://localhost:3000/mcp");
        let repo = Arc::new(MockMcpRepository::with_servers(vec![server]));
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.total, 1);
        let s = &output.servers[0];
        assert_eq!(s.name, "my-server");
        assert_eq!(s.url, "http://localhost:3000/mcp");
        assert!(!s.has_api_key);
        assert!(s.enabled);
    }

    #[tokio::test]
    async fn test_list_masks_api_key() {
        let server = McpServerConfig {
            name: "remote".to_string(),
            url: "https://mcp.example.com/tools".to_string(),
            api_key: Some("sk-super-secret".to_string()),
            enabled: true,
            is_module: false,
        };
        let repo = Arc::new(MockMcpRepository::with_servers(vec![server]));
        let tool = ListMcpTool::new(repo);

        let output = tool.call(ListMcpToolArgs {}).await.unwrap();
        let s = &output.servers[0];
        // API key is never exposed — only whether one is configured
        assert!(s.has_api_key);
    }

    #[tokio::test]
    async fn test_list_multiple_servers() {
        let servers = vec![
            make_server("server-a", "http://localhost:3001/mcp"),
            make_server("server-b", "http://localhost:3002/mcp"),
        ];
        let repo = Arc::new(MockMcpRepository::with_servers(servers));
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.total, 2);
        assert_eq!(output.servers[0].name, "server-a");
        assert_eq!(output.servers[1].name, "server-b");
        assert_eq!(output.servers[0].url, "http://localhost:3001/mcp");
        assert_eq!(output.servers[1].url, "http://localhost:3002/mcp");
    }

    #[tokio::test]
    async fn test_list_returns_correct_fields() {
        let server = make_server("my-server", "http://localhost:3000/mcp");
        let repo = Arc::new(MockMcpRepository::with_servers(vec![server]));
        let tool = ListMcpTool::new(repo);

        let output = tool.call(ListMcpToolArgs {}).await.unwrap();
        let s = &output.servers[0];
        assert_eq!(s.url, "http://localhost:3000/mcp");
        assert!(!s.has_api_key);
        assert!(s.enabled);
    }

    #[tokio::test]
    async fn test_list_disabled_server() {
        let server = McpServerConfig {
            name: "disabled".to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: false,
            is_module: false,
        };
        let repo = Arc::new(MockMcpRepository::with_servers(vec![server]));
        let tool = ListMcpTool::new(repo);

        let output = tool.call(ListMcpToolArgs {}).await.unwrap();
        assert!(!output.servers[0].enabled);
    }

    #[tokio::test]
    async fn test_list_load_error() {
        let repo = Arc::new(MockMcpRepository::with_load_error("disk read failure"));
        let tool = ListMcpTool::new(repo);

        let result = tool.call(ListMcpToolArgs {}).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ToolError::OperationFailed(_)));
    }
}
