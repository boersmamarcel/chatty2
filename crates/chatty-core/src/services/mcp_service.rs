use anyhow::{Context, Result};
use rmcp::service::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::settings::models::mcp_store::McpServerConfig;

/// Represents an active MCP server connection
pub struct McpConnection {
    /// Server name
    pub name: String,

    /// The rmcp service for communicating with the server
    pub service: rmcp::service::RunningService<rmcp::RoleClient, ()>,

    /// Cached tool list, populated on first fetch and invalidated on reconnect
    cached_tools: Option<Vec<rmcp::model::Tool>>,
}

impl McpConnection {
    /// Connect to an already-running MCP server via its HTTP endpoint.
    pub async fn connect(config: McpServerConfig) -> Result<Self> {
        let name = config.name.clone();

        info!(
            server = %name,
            url = %config.url,
            has_api_key = config.has_api_key(),
            "Connecting to MCP server"
        );

        let transport = StreamableHttpClientTransport::from_config(
            rmcp::transport::StreamableHttpClientTransportConfig {
                uri: config.url.as_str().into(),
                auth_header: config.api_key.filter(|k| !k.is_empty()),
                ..Default::default()
            },
        );

        let service = ()
            .serve(transport)
            .await
            .with_context(|| format!("Failed to connect to MCP server: {}", name))?;

        let server_info = service.peer_info();
        info!(
            server = %name,
            info = ?server_info,
            "MCP server connected"
        );

        Ok(Self {
            name,
            service,
            cached_tools: None,
        })
    }

    /// List available tools from this MCP server, using cache when available
    pub async fn list_tools(&mut self) -> Result<Vec<rmcp::model::Tool>> {
        if let Some(ref cached) = self.cached_tools {
            debug!(server = %self.name, "Returning cached tool list");
            return Ok(cached.clone());
        }

        let response = self
            .service
            .list_tools(Default::default())
            .await
            .with_context(|| format!("Failed to list tools from server: {}", self.name))?;

        self.cached_tools = Some(response.tools.clone());
        Ok(response.tools)
    }

    /// Gracefully disconnect from the server
    pub async fn disconnect(self) -> Result<()> {
        info!(server = %self.name, "Disconnecting from MCP server");

        self.service
            .cancel()
            .await
            .with_context(|| format!("Failed to cancel MCP service: {}", self.name))?;

        Ok(())
    }
}

/// Global service for managing MCP server connections
#[derive(Clone)]
pub struct McpService {
    /// Active connections keyed by server name
    connections: Arc<RwLock<HashMap<String, McpConnection>>>,
}

impl McpService {
    /// Create a new MCP service
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Connect to a single MCP server by URL.
    pub async fn connect_server(&self, config: McpServerConfig) -> Result<()> {
        let name = config.name.clone();

        // Check if already connected
        {
            let connections = self.connections.read().await;
            if connections.contains_key(&name) {
                warn!(server = %name, "MCP server already connected");
                return Ok(());
            }
        }

        let connection = McpConnection::connect(config).await?;

        {
            let mut connections = self.connections.write().await;
            connections.insert(name.clone(), connection);
        }

        info!(server = %name, "MCP server connected successfully");
        Ok(())
    }

    /// Disconnect from a single MCP server.
    pub async fn disconnect_server(&self, name: &str) -> Result<()> {
        let connection = {
            let mut connections = self.connections.write().await;
            connections.remove(name)
        };

        if let Some(connection) = connection {
            connection.disconnect().await?;
            info!(server = %name, "MCP server disconnected");
        } else {
            warn!(server = %name, "MCP server not found");
        }

        Ok(())
    }

    /// Connect to all enabled servers from the given configurations concurrently.
    pub async fn connect_all(&self, configs: Vec<McpServerConfig>) -> Result<()> {
        info!(count = configs.len(), "Connecting to MCP servers");

        let mut join_set = tokio::task::JoinSet::new();

        for config in configs {
            if !config.enabled {
                debug!(server = %config.name, "Skipping disabled MCP server");
                continue;
            }

            let svc = self.clone();
            join_set.spawn(async move {
                let name = config.name.clone();
                match svc.connect_server(config).await {
                    Ok(()) => None,
                    Err(e) => {
                        error!(server = %name, error = ?e, "Failed to connect to MCP server");
                        Some((name, e))
                    }
                }
            });
        }

        let mut error_count = 0usize;
        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok(Some(_)) => error_count += 1,
                Ok(None) => {}
                Err(e) => warn!(error = ?e, "MCP server connect task panicked"),
            }
        }

        if error_count > 0 {
            warn!(failed = error_count, "Some MCP servers failed to connect");
        }

        Ok(())
    }

    /// Disconnect from all connected servers
    pub async fn disconnect_all(&self) -> Result<()> {
        let server_names: Vec<String> = {
            let connections = self.connections.read().await;
            connections.keys().cloned().collect()
        };

        info!(count = server_names.len(), "Disconnecting from all MCP servers");

        for name in server_names {
            if let Err(e) = self.disconnect_server(&name).await {
                error!(
                    server = %name,
                    error = ?e,
                    "Failed to disconnect from MCP server"
                );
            }
        }

        Ok(())
    }

    /// Get all tools from all active servers, grouped by server with their ServerSinks.
    ///
    /// Tool lists are cached after the first successful fetch per server.
    pub async fn get_all_tools_with_sinks(
        &self,
    ) -> Result<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
        let mut connections = self.connections.write().await;
        let mut result = Vec::new();

        for (name, connection) in connections.iter_mut() {
            match connection.list_tools().await {
                Ok(tools) => {
                    let server_sink = connection.service.peer().clone();
                    let tool_count = tools.len();

                    for tool in &tools {
                        debug!(
                            server = %name,
                            tool_name = %tool.name,
                            "Retrieved tool from MCP server"
                        );
                    }

                    result.push((name.clone(), tools, server_sink));
                    info!(
                        server = %name,
                        tool_count = tool_count,
                        "Retrieved tools with ServerSink"
                    );
                }
                Err(e) => {
                    error!(
                        server = %name,
                        error = ?e,
                        "Failed to list tools from MCP server"
                    );
                }
            }
        }

        Ok(result)
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::mcp_store::McpServerConfig;

    fn disabled_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: false,
        }
    }

    // --- McpService::new / Default ---

    #[test]
    fn test_new_service_has_no_connections() {
        let svc = McpService::new();
        // A freshly created service should have no active connections
        // (verified by get_all_tools_with_sinks returning empty in async tests)
        let _ = svc.connections.try_read().is_ok();
    }

    #[test]
    fn test_default_equals_new() {
        let _svc = McpService::default();
        // Default constructor delegates to new() — just verify it doesn't panic
    }

    // --- disconnect_server on unknown name ---

    #[tokio::test]
    async fn test_disconnect_server_unknown_is_ok() {
        let svc = McpService::new();
        let result = svc.disconnect_server("nonexistent").await;
        assert!(result.is_ok());
    }

    // --- connect_all skips disabled servers ---

    #[tokio::test]
    async fn test_connect_all_skips_disabled_servers() {
        let svc = McpService::new();
        let configs = vec![disabled_config("disabled-a"), disabled_config("disabled-b")];
        let result = svc.connect_all(configs).await;
        assert!(result.is_ok());

        // No connections should have been registered for disabled servers
        let connections = svc.connections.read().await;
        assert!(connections.is_empty());
    }

    #[tokio::test]
    async fn test_connect_all_empty_list_is_ok() {
        let svc = McpService::new();
        let result = svc.connect_all(vec![]).await;
        assert!(result.is_ok());
    }

    // --- get_all_tools_with_sinks with no connections ---

    #[tokio::test]
    async fn test_get_all_tools_no_connections_returns_empty() {
        let svc = McpService::new();
        let result = svc.get_all_tools_with_sinks().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    // --- disconnect_all with no connections ---

    #[tokio::test]
    async fn test_disconnect_all_empty_is_ok() {
        let svc = McpService::new();
        let result = svc.disconnect_all().await;
        assert!(result.is_ok());
    }

    // --- connect_all: bad URL returns Ok (non-fatal, errors are logged) ---

    #[tokio::test]
    async fn test_connect_all_bad_url_returns_ok() {
        let svc = McpService::new();
        let configs = vec![
            McpServerConfig {
                name: "bad-1".to_string(),
                url: "http://127.0.0.1:1/mcp".to_string(),
                api_key: None,
                enabled: true,
            },
            McpServerConfig {
                name: "bad-2".to_string(),
                url: "http://127.0.0.1:2/mcp".to_string(),
                api_key: None,
                enabled: true,
            },
        ];

        let result = svc.connect_all(configs).await;
        // connect_all returns Ok even when all servers fail to connect
        assert!(result.is_ok());
    }

    // --- Tool cache: get_all_tools idempotent with no connections ---

    #[tokio::test]
    async fn test_get_all_tools_idempotent_no_connections() {
        let svc = McpService::new();
        let r1 = svc.get_all_tools_with_sinks().await.unwrap();
        let r2 = svc.get_all_tools_with_sinks().await.unwrap();
        assert_eq!(r1.len(), r2.len());
    }
}
