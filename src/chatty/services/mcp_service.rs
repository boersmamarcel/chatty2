use anyhow::{Context, Result};
use gpui::Global;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rmcp::service::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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

    /// Process ID of the child process (if available)
    pub pid: Option<u32>,

    /// Cached tool list, populated on first fetch and invalidated on restart
    cached_tools: Option<Vec<rmcp::model::Tool>>,
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
            pid,
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

    /// Gracefully shutdown the connection
    pub async fn shutdown(self) -> Result<()> {
        info!(server = %self.name, "Shutting down MCP server");

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

    /// PID index for synchronous access during shutdown (std Mutex for sync use)
    pids: Arc<Mutex<HashMap<String, u32>>>,
}

impl Global for McpService {}

impl McpService {
    /// Create a new MCP service
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            pids: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Synchronously send SIGTERM to all child processes.
    ///
    /// This is called from the synchronous quit handler before cx.quit() to give
    /// child MCP processes a best-effort signal before the process exits. The async
    /// stop_all() task is also spawned for graceful shutdown, but may not complete
    /// before the process terminates.
    pub fn kill_all_sync(&self) {
        let pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
        for (name, pid) in pids.iter() {
            let pid_i32 = *pid as i32;
            match kill(Pid::from_raw(pid_i32), Signal::SIGTERM) {
                Ok(()) => info!(server = %name, pid = pid_i32, "Sent SIGTERM to MCP server"),
                Err(e) => warn!(server = %name, pid = pid_i32, error = ?e, "Failed to send SIGTERM to MCP server"),
            }
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

        // Track PID for synchronous shutdown
        if let Some(pid) = connection.pid {
            let mut pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
            pids.insert(name.clone(), pid);
        }

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

        // Remove from PID index
        {
            let mut pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
            pids.remove(name);
        }

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
    /// Get all tools from all active servers, grouped by server with their ServerSinks.
    ///
    /// Tool lists are cached after the first successful fetch per server and
    /// invalidated when a server is restarted via restart_server().
    pub async fn get_all_tools_with_sinks(&self) -> Result<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
        let mut connections = self.connections.write().await;
        let mut result = Vec::new();

        for (name, connection) in connections.iter_mut() {
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

}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
}
