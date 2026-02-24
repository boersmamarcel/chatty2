use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::chatty::models::execution_approval_store::{
    PendingApprovals, request_execution_approval,
};
use crate::chatty::services::shell_service::{ShellOutput, ShellSession, ShellStatus};
use crate::settings::models::execution_settings::ExecutionSettingsModel;

// ── Error types ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ShellToolError {
    #[error("Shell error: {0}")]
    ShellError(#[from] anyhow::Error),
}

// ── ShellExecuteTool ─────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ShellExecuteArgs {
    pub command: String,
}

#[derive(Debug, Serialize)]
pub struct ShellExecuteOutput {
    pub stdout: String,
    pub exit_code: i32,
    pub truncated: bool,
}

impl From<ShellOutput> for ShellExecuteOutput {
    fn from(o: ShellOutput) -> Self {
        Self {
            stdout: o.stdout,
            exit_code: o.exit_code,
            truncated: o.truncated,
        }
    }
}

/// Execute a command in a persistent shell session that preserves state.
#[derive(Clone)]
pub struct ShellExecuteTool {
    session: Arc<ShellSession>,
    settings: ExecutionSettingsModel,
    pending_approvals: PendingApprovals,
}

impl ShellExecuteTool {
    pub fn new(
        session: Arc<ShellSession>,
        settings: ExecutionSettingsModel,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            session,
            settings,
            pending_approvals,
        }
    }

    async fn request_approval(&self, command: &str) -> anyhow::Result<bool> {
        let is_sandboxed = self.session.is_sandboxed().await;
        request_execution_approval(
            &self.pending_approvals,
            &self.settings.approval_mode,
            &format!("[shell] {}", command),
            is_sandboxed,
        )
        .await
    }
}

impl Tool for ShellExecuteTool {
    const NAME: &'static str = "shell_execute";
    type Error = ShellToolError;
    type Args = ShellExecuteArgs;
    type Output = ShellExecuteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "shell_execute".to_string(),
            description: "Execute a command in a persistent shell session. Unlike the 'bash' tool which \
                         runs each command in a fresh process, this tool maintains state across invocations: \
                         environment variables, working directory, and shell history persist between calls. \
                         \
                         Use this when you need to:\n\
                         - Build up environment state across multiple commands\n\
                         - Run commands that depend on previous shell state\n\
                         - Work in a specific directory across multiple operations\n\
                         \
                         The session is per-conversation and automatically cleaned up when the conversation ends."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to execute in the persistent shell session"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.settings.enabled {
            return Err(ShellToolError::ShellError(anyhow::anyhow!(
                "Code execution is disabled. Enable it in Settings → Execution."
            )));
        }

        let approved = self.request_approval(&args.command).await?;
        if !approved {
            return Err(ShellToolError::ShellError(anyhow::anyhow!(
                "Execution denied by user"
            )));
        }

        tracing::debug!(command = %args.command, "Executing in shell session");
        let output = self.session.execute(&args.command).await?;
        Ok(output.into())
    }
}

// ── ShellSetEnvTool ──────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ShellSetEnvArgs {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct ShellSetEnvOutput {
    pub success: bool,
    pub message: String,
}

/// Set an environment variable in the persistent shell session.
#[derive(Clone)]
pub struct ShellSetEnvTool {
    session: Arc<ShellSession>,
    settings: ExecutionSettingsModel,
}

impl ShellSetEnvTool {
    pub fn new(session: Arc<ShellSession>, settings: ExecutionSettingsModel) -> Self {
        Self { session, settings }
    }
}

impl Tool for ShellSetEnvTool {
    const NAME: &'static str = "shell_set_env";
    type Error = ShellToolError;
    type Args = ShellSetEnvArgs;
    type Output = ShellSetEnvOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "shell_set_env".to_string(),
            description: "Set an environment variable in the persistent shell session. \
                         The variable will be available to all subsequent commands in this session."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The environment variable name (alphanumeric and underscore only)"
                    },
                    "value": {
                        "type": "string",
                        "description": "The value to set"
                    }
                },
                "required": ["key", "value"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.settings.enabled {
            return Err(ShellToolError::ShellError(anyhow::anyhow!(
                "Code execution is disabled. Enable it in Settings → Execution."
            )));
        }

        tracing::debug!(key = %args.key, "Setting env var in shell session");
        match self.session.set_env(&args.key, &args.value).await {
            Ok(_) => Ok(ShellSetEnvOutput {
                success: true,
                message: format!("Environment variable '{}' set successfully", args.key),
            }),
            Err(e) => Ok(ShellSetEnvOutput {
                success: false,
                message: format!("Failed to set environment variable: {}", e),
            }),
        }
    }
}

// ── ShellCdTool ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ShellCdArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ShellCdOutput {
    pub success: bool,
    pub cwd: String,
    pub message: String,
}

/// Change the working directory in the persistent shell session.
#[derive(Clone)]
pub struct ShellCdTool {
    session: Arc<ShellSession>,
    settings: ExecutionSettingsModel,
}

impl ShellCdTool {
    pub fn new(session: Arc<ShellSession>, settings: ExecutionSettingsModel) -> Self {
        Self { session, settings }
    }
}

impl Tool for ShellCdTool {
    const NAME: &'static str = "shell_cd";
    type Error = ShellToolError;
    type Args = ShellCdArgs;
    type Output = ShellCdOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "shell_cd".to_string(),
            description: "Change the working directory in the persistent shell session. \
                         The new directory will persist for all subsequent commands. \
                         If a workspace is configured, the path must stay within the workspace bounds."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The directory path to change to (absolute or relative)"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.settings.enabled {
            return Err(ShellToolError::ShellError(anyhow::anyhow!(
                "Code execution is disabled. Enable it in Settings → Execution."
            )));
        }

        tracing::debug!(path = %args.path, "Changing directory in shell session");
        match self.session.cd(&args.path).await {
            Ok(_) => {
                // Get the actual cwd after cd
                let cwd = self
                    .session
                    .execute("pwd")
                    .await
                    .map(|o| o.stdout.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());

                Ok(ShellCdOutput {
                    success: true,
                    cwd,
                    message: format!("Changed directory to '{}'", args.path),
                })
            }
            Err(e) => Ok(ShellCdOutput {
                success: false,
                cwd: "unchanged".to_string(),
                message: format!("Failed to change directory: {}", e),
            }),
        }
    }
}

// ── ShellStatusTool ──────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ShellStatusArgs {}

#[derive(Debug, Serialize)]
pub struct ShellStatusOutput {
    pub running: bool,
    pub cwd: String,
    pub env_vars: Vec<(String, String)>,
    pub pid: Option<u32>,
    pub uptime_seconds: u64,
}

impl From<ShellStatus> for ShellStatusOutput {
    fn from(s: ShellStatus) -> Self {
        Self {
            running: s.running,
            cwd: s.cwd,
            env_vars: s.env_vars,
            pid: s.pid,
            uptime_seconds: s.uptime_seconds,
        }
    }
}

/// Query the current state of the persistent shell session.
#[derive(Clone)]
pub struct ShellStatusTool {
    session: Arc<ShellSession>,
}

impl ShellStatusTool {
    pub fn new(session: Arc<ShellSession>) -> Self {
        Self { session }
    }
}

impl Tool for ShellStatusTool {
    const NAME: &'static str = "shell_status";
    type Error = ShellToolError;
    type Args = ShellStatusArgs;
    type Output = ShellStatusOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "shell_status".to_string(),
            description: "Get the current status of the persistent shell session, including \
                         working directory, environment variables, process ID, and uptime."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::debug!("Querying shell session status");
        let status = self.session.status().await?;
        Ok(status.into())
    }
}
