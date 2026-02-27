use anyhow::{Context, Result};
use gpui::Global;
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
use rmcp::service::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
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
    ///
    /// Captures stderr from the child process so that if initialization fails
    /// (e.g. "connection closed: initialize response"), the error message
    /// includes the server's stderr output for diagnostics.
    ///
    /// Initialization is given a 30-second timeout to prevent hanging on
    /// servers that start but never complete the MCP handshake.
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

        // Spawn the child process with stderr piped so we can capture
        // diagnostic output if initialization fails.
        let (transport, stderr) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server: {}", name))?;

        // Get process ID before serving
        let pid = transport.id();

        // Initialize the MCP handshake with a timeout.
        // Many servers fail silently (exit before responding to initialize),
        // which would otherwise hang forever.
        const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        let init_result = tokio::time::timeout(INIT_TIMEOUT, ().serve(transport)).await;

        let service = match init_result {
            Ok(Ok(service)) => service,
            Ok(Err(init_err)) => {
                // Initialization failed — read stderr for diagnostics
                let stderr_output = read_stderr(stderr).await;
                let mut msg = format!("Failed to initialize MCP server: {name}");
                if !stderr_output.is_empty() {
                    msg.push_str(&format!("\n--- server stderr ---\n{stderr_output}"));
                }
                return Err(anyhow::anyhow!(msg).context(init_err.to_string()));
            }
            Err(_elapsed) => {
                let stderr_output = read_stderr(stderr).await;
                let mut msg = format!(
                    "MCP server '{name}' timed out during initialization ({INIT_TIMEOUT:?})"
                );
                if !stderr_output.is_empty() {
                    msg.push_str(&format!("\n--- server stderr ---\n{stderr_output}"));
                }
                return Err(anyhow::anyhow!(msg));
            }
        };

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

/// Read up to 4 KiB of stderr from a child process, best-effort.
/// Returns an empty string if stderr is `None` or unreadable.
async fn read_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(std::time::Duration::from_millis(500), stderr.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => String::from_utf8_lossy(&buf[..n]).trim().to_string(),
        _ => String::new(),
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

    /// Synchronously send SIGTERM (Unix) or taskkill (Windows) to all child processes.
    ///
    /// This is called from the synchronous quit handler before cx.quit() to give
    /// child MCP processes a best-effort signal before the process exits.
    pub fn kill_all_sync(&self) {
        let pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());

        for (name, pid) in pids.iter() {
            #[cfg(unix)]
            {
                let pid_i32 = *pid as i32;
                match kill(Pid::from_raw(pid_i32), Signal::SIGTERM) {
                    Ok(()) => info!(server = %name, pid = pid_i32, "Sent SIGTERM to MCP server"),
                    Err(e) => {
                        warn!(server = %name, pid = pid_i32, error = ?e, "Failed to send SIGTERM to MCP server")
                    }
                }
            }

            #[cfg(windows)]
            {
                // On Windows, use `taskkill` to forcefully terminate the process.
                // We use std::process::Command here because we are in a sync context.
                let pid_str = pid.to_string();
                match std::process::Command::new("taskkill")
                    .arg("/F") // Forcefully terminate the process
                    .arg("/PID") // Specify Process ID
                    .arg(&pid_str)
                    // We capture output to avoid polluting stdout/stderr unless there's an error
                    .output()
                {
                    Ok(output) => {
                        if output.status.success() {
                            info!(server = %name, pid = pid, "Sent taskkill /F to MCP server");
                        } else {
                            // Convert stderr to string for logging if it failed (e.g., access denied or process gone)
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            warn!(server = %name, pid = pid, error = %stderr, "Failed to taskkill MCP server");
                        }
                    }
                    Err(e) => {
                        warn!(server = %name, pid = pid, error = ?e, "Failed to execute taskkill command");
                    }
                }
            }

            #[cfg(not(any(unix, windows)))]
            {
                // Fallback for other platforms (e.g. WASM, Redox)
                warn!(server = %name, pid = pid, "Skipping synchronous kill: unsupported platform");
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
            warn!(failed = errors.len(), "Some MCP servers failed to start");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::mcp_store::McpServerConfig;
    use std::collections::HashMap;

    fn disabled_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "true".to_string(),
            args: vec![],
            env: HashMap::new(),
            enabled: false,
        }
    }

    // --- McpService::new / Default ---

    #[test]
    fn test_new_service_has_no_connections() {
        let svc = McpService::new();
        let pids = svc.pids.lock().unwrap();
        assert!(pids.is_empty());
    }

    #[test]
    fn test_default_equals_new() {
        let svc = McpService::default();
        let pids = svc.pids.lock().unwrap();
        assert!(pids.is_empty());
    }

    // --- kill_all_sync with empty pids ---

    #[test]
    fn test_kill_all_sync_empty_does_not_panic() {
        let svc = McpService::new();
        // Should complete without panicking when no pids are tracked
        svc.kill_all_sync();
    }

    // --- stop_server on unknown name ---

    #[tokio::test]
    async fn test_stop_server_unknown_is_ok() {
        let svc = McpService::new();
        // Stopping a server that was never started should succeed (logs a warning)
        let result = svc.stop_server("nonexistent").await;
        assert!(result.is_ok());
    }

    // --- start_all skips disabled servers ---

    #[tokio::test]
    async fn test_start_all_skips_disabled_servers() {
        let svc = McpService::new();
        let configs = vec![disabled_config("disabled-a"), disabled_config("disabled-b")];
        let result = svc.start_all(configs).await;
        assert!(result.is_ok());

        // No connections should have been registered for disabled servers
        let connections = svc.connections.read().await;
        assert!(connections.is_empty());

        let pids = svc.pids.lock().unwrap();
        assert!(pids.is_empty());
    }

    #[tokio::test]
    async fn test_start_all_empty_list_is_ok() {
        let svc = McpService::new();
        let result = svc.start_all(vec![]).await;
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

    // --- stop_all with no connections ---

    #[tokio::test]
    async fn test_stop_all_empty_is_ok() {
        let svc = McpService::new();
        let result = svc.stop_all().await;
        assert!(result.is_ok());
    }

    // --- start_server with a bad command ---
    //
    // We cannot construct a McpConnection without spawn(), so we verify the
    // error path: a command that does not exist should cause start_server to
    // return Err and leave the connections map empty.

    #[tokio::test]
    async fn test_start_server_bad_command_returns_err_and_no_connection() {
        let svc = McpService::new();
        let bad_config = McpServerConfig {
            name: "bad-server".to_string(),
            command: "/nonexistent/command".to_string(),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
        };

        let result = svc.start_server(bad_config).await;
        assert!(result.is_err(), "Expected Err for nonexistent command");

        // Failed spawn must not leave a partial connection
        let connections = svc.connections.read().await;
        assert!(connections.is_empty());

        let pids = svc.pids.lock().unwrap();
        assert!(pids.is_empty());
    }

    // --- Verify pids are cleaned up on stop_server ---
    //
    // Manually pre-insert a PID (simulating a partially-cleaned state where the
    // connection was already removed but the pid index was not), then verify
    // stop_server clears it.

    #[tokio::test]
    async fn test_stop_server_clears_pid_entry() {
        let svc = McpService::new();

        // Manually insert a fake PID entry
        {
            let mut pids = svc.pids.lock().unwrap();
            pids.insert("fake-server".to_string(), 99999);
        }

        // stop_server should remove the pid even though no live connection exists
        let result = svc.stop_server("fake-server").await;
        assert!(result.is_ok());

        let pids = svc.pids.lock().unwrap();
        assert!(
            !pids.contains_key("fake-server"),
            "PID entry should be removed after stop_server"
        );
    }

    // --- Tool cache: get_all_tools idempotent with no connections ---
    //
    // McpConnection::spawn requires a real MCP-compatible process, so we verify
    // the observable: repeated calls to get_all_tools_with_sinks are idempotent
    // when there are no connections.

    #[tokio::test]
    async fn test_get_all_tools_idempotent_no_connections() {
        let svc = McpService::new();
        let r1 = svc.get_all_tools_with_sinks().await.unwrap();
        let r2 = svc.get_all_tools_with_sinks().await.unwrap();
        assert_eq!(r1.len(), r2.len());
    }

    // --- start_all: partial failure returns Ok (non-fatal) ---
    //
    // start_all is designed to continue starting remaining servers when one
    // fails; it returns Ok(()) even if some servers fail, only logging errors.

    #[tokio::test]
    async fn test_start_all_partial_failure_returns_ok() {
        let svc = McpService::new();
        let configs = vec![
            McpServerConfig {
                name: "bad-1".to_string(),
                command: "/no/such/binary".to_string(),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
            },
            McpServerConfig {
                name: "bad-2".to_string(),
                command: "/also/missing".to_string(),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
            },
        ];

        let result = svc.start_all(configs).await;
        // start_all returns Ok even when all servers fail
        assert!(result.is_ok());
    }
}
