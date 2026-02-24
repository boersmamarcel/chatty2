use gpui::Global;
use serde::{Deserialize, Serialize};

/// Approval mode for code execution requests
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum ApprovalMode {
    /// Require approval for all commands (default, most secure)
    #[default]
    AlwaysAsk,
    /// Auto-approve sandboxed commands, ask for unsandboxed
    AutoApproveSandboxed,
    /// Auto-approve all commands (dangerous, opt-in only)
    AutoApproveAll,
}

/// Settings for code execution tool
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionSettingsModel {
    /// Master toggle for code execution feature
    pub enabled: bool,
    /// Approval behavior for command execution
    pub approval_mode: ApprovalMode,
    /// Working directory for commands (None = current directory)
    pub workspace_dir: Option<String>,
    /// Enable filesystem read tools (requires workspace_dir to be set)
    #[serde(default = "default_true")]
    pub filesystem_read_enabled: bool,
    /// Enable filesystem write tools (requires workspace_dir to be set)
    #[serde(default = "default_true")]
    pub filesystem_write_enabled: bool,
    /// Enable the add_mcp_service tool, which allows the LLM to register new MCP servers.
    /// Opt-in: disabled by default to prevent the AI from adding new command-line integrations
    /// without explicit user action.
    #[serde(default)]
    pub mcp_service_tool_enabled: bool,
    /// Enable the built-in fetch tool, which allows the LLM to make read-only HTTP GET requests.
    /// Zero-configuration web access without requiring an MCP fetch server.
    #[serde(default = "default_true")]
    pub fetch_enabled: bool,
    /// Enable git integration tools (status, diff, log, branch, commit).
    /// Requires workspace_dir to be set and the workspace to be a git repository.
    #[serde(default)]
    pub git_enabled: bool,
    /// Maximum execution time in seconds
    pub timeout_seconds: u32,
    /// Maximum output size in bytes (prevents memory exhaustion)
    pub max_output_bytes: usize,
    /// Enable network isolation in sandbox (when available)
    pub network_isolation: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ExecutionSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in by default for security
            approval_mode: ApprovalMode::AlwaysAsk,
            workspace_dir: None,
            filesystem_read_enabled: true, // Enabled by default when workspace is set
            filesystem_write_enabled: true, // Enabled by default when workspace is set
            mcp_service_tool_enabled: false,
            fetch_enabled: true, // Enabled by default for zero-config web access
            git_enabled: false,  // Opt-in: requires workspace with git repo
            timeout_seconds: 30,
            max_output_bytes: 51200, // 50KB
            network_isolation: false,
        }
    }
}

impl Global for ExecutionSettingsModel {}
