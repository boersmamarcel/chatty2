use anyhow::{Context, Result};
use gpui::Global;
use rmcp::service::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::settings::models::mcp_store::McpServerConfig;

/// Represents an active MCP server connection
pub struct McpConnection {
    /// Server name
    pub name: String,
    
    /// The rmcp service for communicating with the server
    pub service: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    
    /// Configuration used to spawn this server
    pub config: McpServerConfig,
    
    /// Process ID of the child process (if available)
    pub pid: Option<u32>,
}

impl McpConnection {
    /// Create a new MCP connection by spawning a child process
    pub async fn spawn(config: McpServerConfig) -> Result<Self> {
        let name = config.name.clone();
        
        info!(
            server = %name,
            command = %config.command,
            args = ?config.args,
            "Spawning MCP server"
        );

        // Build and configure the command
        let cmd = Command::new(&config.command).configure(|cmd| {
            cmd.args(&config.args);
            for (key, value) in &config.env {
                cmd.env(key, value);
            }
        });

        // Spawn the child process
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("Failed to spawn MCP server: {}", name))?;

        // Get process ID before serving
        let pid = transport.id();

        // Create the service
        let service = ()
            .serve(transport)
            .await
            .with_context(|| format!("Failed to initialize MCP service: {}", name))?;

        // Log server info
        let server_info = service.peer_info();
        info!(
            server = %name,
            pid = ?pid,
            info = ?server_info,
            "MCP server connected"
        );

        Ok(Self {
            name: name.clone(),
            service,
            config,
            pid,
        })
    }

    /// List available tools from this MCP server
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>> {
        let response = self
            .service
            .list_tools(Default::default())
            .await
            .with_context(|| format!("Failed to list tools from server: {}", self.name))?;

        Ok(response.tools)
    }

    /// Gracefully shutdown the connection
    pub async fn shutdown(mut self) -> Result<()> {
        info!(server = %self.name, "Shutting down MCP server");
        
        self.service
            .cancel()
            .await
            .with_context(|| format!("Failed to cancel MCP service: {}", self.name))?;

        Ok(())
    }

    /// Check if the connection is healthy by listing tools
    pub async fn health_check(&self) -> bool {
        debug!(server = %self.name, "Performing health check");
        self.service.list_tools(Default::default()).await.is_ok()
    }
}

/// Global service for managing MCP server connections
#[derive(Clone)]
pub struct McpService {
    /// Active connections keyed by server name
    connections: Arc<RwLock<HashMap<String, McpConnection>>>,
}

impl Global for McpService {}

impl McpService {
    /// Create a new MCP service
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start a single MCP server
    pub async fn start_server(&self, config: McpServerConfig) -> Result<()> {
        let name = config.name.clone();

        // Check if already running
        {
            let connections = self.connections.read().await;
            if connections.contains_key(&name) {
                warn!(server = %name, "MCP server already running");
                return Ok(());
            }
        }

        // Spawn the connection
        let connection = McpConnection::spawn(config).await?;

        // Store the connection
        {
            let mut connections = self.connections.write().await;
            connections.insert(name.clone(), connection);
        }

        info!(server = %name, "MCP server started successfully");
        Ok(())
    }

    /// Stop a single MCP server
    pub async fn stop_server(&self, name: &str) -> Result<()> {
        let connection = {
            let mut connections = self.connections.write().await;
            connections.remove(name)
        };

        if let Some(connection) = connection {
            connection.shutdown().await?;
            info!(server = %name, "MCP server stopped");
        } else {
            warn!(server = %name, "MCP server not found");
        }

        Ok(())
    }

    /// Start all enabled servers from the given configurations
    pub async fn start_all(&self, configs: Vec<McpServerConfig>) -> Result<()> {
        info!(count = configs.len(), "Starting MCP servers");

        let mut errors = Vec::new();

        for config in configs {
            if !config.enabled {
                debug!(server = %config.name, "Skipping disabled MCP server");
                continue;
            }

            if let Err(e) = self.start_server(config.clone()).await {
                error!(
                    server = %config.name,
                    error = ?e,
                    "Failed to start MCP server"
                );
                errors.push((config.name.clone(), e));
            }
        }

        if !errors.is_empty() {
            warn!(
                failed = errors.len(),
                "Some MCP servers failed to start"
            );
        }

        Ok(())
    }

    /// Stop all running servers
    pub async fn stop_all(&self) -> Result<()> {
        let server_names: Vec<String> = {
            let connections = self.connections.read().await;
            connections.keys().cloned().collect()
        };

        info!(count = server_names.len(), "Stopping all MCP servers");

        for name in server_names {
            if let Err(e) = self.stop_server(&name).await {
                error!(
                    server = %name,
                    error = ?e,
                    "Failed to stop MCP server"
                );
            }
        }

        Ok(())
    }

    /// Get a list of all active server names
    pub async fn active_servers(&self) -> Vec<String> {
        let connections = self.connections.read().await;
        connections.keys().cloned().collect()
    }

    /// Check if a server is running
    pub async fn is_running(&self, name: &str) -> bool {
        let connections = self.connections.read().await;
        connections.contains_key(name)
    }

    /// Get all tools from all active servers
    pub async fn list_all_tools(&self) -> Result<HashMap<String, Vec<rmcp::model::Tool>>> {
        let connections = self.connections.read().await;
        let mut all_tools = HashMap::new();

        for (name, connection) in connections.iter() {
            match connection.list_tools().await {
                Ok(tools) => {
                    debug!(
                        server = %name,
                        tool_count = tools.len(),
                        "Listed tools from MCP server"
                    );
                    all_tools.insert(name.clone(), tools);
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

        Ok(all_tools)
    }

    /// Restart a server (stop then start)
    pub async fn restart_server(&self, config: McpServerConfig) -> Result<()> {
        let name = config.name.clone();
        info!(server = %name, "Restarting MCP server");

        // Stop if running
        if self.is_running(&name).await {
            self.stop_server(&name).await?;
        }

        // Start with new config
        self.start_server(config).await?;

        Ok(())
    }

    /// Get tools from a specific server with its ServerSink
    pub async fn get_server_tools(&self, name: &str) -> Result<Option<(Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
        let connections = self.connections.read().await;
        
        if let Some(connection) = connections.get(name) {
            let tools = connection.list_tools().await?;
            let server_sink = connection.service.peer().clone();
            Ok(Some((tools, server_sink)))
        } else {
            Ok(None)
        }
    }

    /// Get all tools from all active servers, grouped by server with their ServerSinks
    pub async fn get_all_tools_with_sinks(&self) -> Result<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
        let connections = self.connections.read().await;
        let mut result = Vec::new();

        for (name, connection) in connections.iter() {
            match connection.list_tools().await {
                Ok(tools) => {
                    let server_sink = connection.service.peer().clone();
                    let tool_count = tools.len();
                    
                    // Log individual tool names for debugging
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

    /// Perform health check on all active connections
    /// Returns list of server names that failed health check
    pub async fn health_check_all(&self) -> Vec<String> {
        let connections = self.connections.read().await;
        let mut failed_servers = Vec::new();

        for (name, connection) in connections.iter() {
            if !connection.health_check().await {
                warn!(server = %name, "Health check failed");
                failed_servers.push(name.clone());
            } else {
                debug!(server = %name, "Health check passed");
            }
        }

        failed_servers
    }

    /// Restart servers that failed health check
    pub async fn restart_unhealthy_servers(&self, configs: &[McpServerConfig]) -> Result<()> {
        let failed_servers = self.health_check_all().await;

        if failed_servers.is_empty() {
            debug!("All MCP servers healthy");
            return Ok(());
        }

        info!(failed = failed_servers.len(), "Restarting unhealthy MCP servers");

        for server_name in failed_servers {
            if let Some(config) = configs.iter().find(|c| c.name == server_name)
                && let Err(e) = self.restart_server(config.clone()).await {
                    error!(
                        server = %server_name,
                        error = ?e,
                        "Failed to restart unhealthy server"
                    );
                }
        }

        Ok(())
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
}
