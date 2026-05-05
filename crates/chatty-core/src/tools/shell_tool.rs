use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::execution_approval_store::{PendingApprovals, request_execution_approval};
use crate::models::message_types::ExecutionEngine;
use crate::services::shell_service::{ShellOutput, ShellSession, ShellStatus};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::tools::ToolError;

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
    pub execution_engine: ExecutionEngine,
}

impl From<ShellOutput> for ShellExecuteOutput {
    fn from(o: ShellOutput) -> Self {
        Self {
            stdout: o.stdout,
            exit_code: o.exit_code,
            truncated: o.truncated,
            execution_engine: ExecutionEngine::Shell,
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
    type Error = ToolError;
    type Args = ShellExecuteArgs;
    type Output = ShellExecuteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "shell_execute".to_string(),
            description: "Execute a command in a persistent shell session. Unlike the 'bash' tool which \
                         runs each command in a fresh process, this tool maintains state across invocations: \
                         environment variables, working directory, and shell history persist between calls. \
                         \
                         User-configured environment secrets are pre-loaded into the session. Scripts can \
                         access them via standard environment variable lookups (e.g. os.environ in Python). \
                         Use shell_status to see which secret variables are available (values are masked). \
                         \
                         For multi-line Python or shell logic, prefer writing a script with a here-doc or \
                         temp file and then running it, instead of very large `python -c '...'` or \
                         `bash -c '...'` one-liners. \
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
            return Err(ToolError::OperationFailed(
                "Code execution is disabled. Enable it in Settings → Execution.".to_string(),
            ));
        }

        let approved = self.request_approval(&args.command).await?;
        if !approved {
            return Err(ToolError::OperationFailed(
                "Execution denied by user".to_string(),
            ));
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
    type Error = ToolError;
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
            return Err(ToolError::OperationFailed(
                "Code execution is disabled. Enable it in Settings → Execution.".to_string(),
            ));
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
    type Error = ToolError;
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
            return Err(ToolError::OperationFailed(
                "Code execution is disabled. Enable it in Settings → Execution.".to_string(),
            ));
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
    type Error = ToolError;
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
        let secret_keys = self.session.secret_key_names();

        // Mask user secret values so the LLM sees keys but not actual values
        let env_vars = status
            .env_vars
            .into_iter()
            .map(|(k, v)| {
                if secret_keys.contains(&k) {
                    (k, "****".to_string())
                } else {
                    (k, v)
                }
            })
            .collect();

        Ok(ShellStatusOutput {
            running: status.running,
            cwd: status.cwd,
            env_vars,
            pid: status.pid,
            uptime_seconds: status.uptime_seconds,
        })
    }
}
