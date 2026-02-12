use gpui::Global;
use serde::{Deserialize, Serialize};

/// Approval mode for code execution requests
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ApprovalMode {
    /// Require approval for all commands (default, most secure)
    AlwaysAsk,
    /// Auto-approve sandboxed commands, ask for unsandboxed
    AutoApproveSandboxed,
    /// Auto-approve all commands (dangerous, opt-in only)
    AutoApproveAll,
}

impl Default for ApprovalMode {
    fn default() -> Self {
        Self::AlwaysAsk
    }
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
    /// Maximum execution time in seconds
    pub timeout_seconds: u32,
    /// Maximum output size in bytes (prevents memory exhaustion)
    pub max_output_bytes: usize,
    /// Enable network isolation in sandbox (when available)
    pub network_isolation: bool,
}

impl Default for ExecutionSettingsModel {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in by default for security
            approval_mode: ApprovalMode::AlwaysAsk,
            workspace_dir: None,
            timeout_seconds: 30,
            max_output_bytes: 51200, // 50KB
            network_isolation: true,
        }
    }
}

impl Global for ExecutionSettingsModel {}
