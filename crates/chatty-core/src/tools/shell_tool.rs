use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::execution_approval_store::{PendingApprovals, request_execution_approval};
use crate::models::message_types::ExecutionEngine;
use crate::services::shell_service::{
    MAX_SHELL_CALL_TIMEOUT_SECONDS, ShellOutput, ShellSession, ShellStatus,
};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::tools::ToolError;

// ── ShellExecuteTool ─────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct ShellExecuteArgs {
    pub command: String,
    /// Optional per-call timeout override in seconds, bounded by
    /// [`MAX_SHELL_CALL_TIMEOUT_SECONDS`]. Defaults to the configured
    /// execution timeout when omitted. Read leniently (see
    /// [`lenient_timeout_seconds`]): a malformed value must not fail the call.
    #[serde(
        default,
        deserialize_with = "lenient_timeout_seconds",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout_seconds: Option<u32>,
}

/// `timeout_seconds` as the model wrote it: an integer, a float (`300.0`),
/// or a numeric string (`"300"`) all count; anything else (`"5m"`, a
/// negative number, `null`) is treated as absent, so the command still runs
/// with the default timeout instead of the whole call failing on argument
/// parsing.
fn lenient_timeout_seconds<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let seconds = match value {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    Ok(seconds
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(|s| s.ceil().min(u32::MAX as f64) as u32))
}

#[derive(Debug, Serialize)]
pub struct ShellExecuteOutput {
    pub stdout: String,
    pub exit_code: i32,
    pub truncated: bool,
    /// Left out when false, so an ordinary command's result is the same
    /// bytes it was before timeouts returned partial output.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    pub execution_engine: ExecutionEngine,
}

impl From<ShellOutput> for ShellExecuteOutput {
    fn from(o: ShellOutput) -> Self {
        Self {
            stdout: o.stdout,
            exit_code: o.exit_code,
            truncated: o.truncated,
            timed_out: o.timed_out,
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

    fn description(&self) -> String {
        "Execute a command in a persistent shell session. Unlike the 'bash' tool which \
                         runs each command in a fresh process, this tool maintains state across invocations: \
                         environment variables, working directory, and shell history persist between calls. \
                         \
                         User-configured environment secrets are pre-loaded into the session. Scripts can \
                         access them via standard environment variable lookups (e.g. os.environ in Python). \
                         Use shell_status to see which secret variables are available (values are masked). \
                         \
                         For multi-line Python or shell logic, prefer writing a script with a here-doc or \
                         temp file and then running it, instead of very large `python -c '...'` or \
                         `bash -c '...'` one-liners. For verbose commands, prefer quiet flags and \
                         targeted output (`curl -fsSL`, `head`, `sed -n`, etc.) rather than dumping \
                         large logs into the session. \
                         \
                         Use this when you need to:\n\
                         - Build up environment state across multiple commands\n\
                         - Run commands that depend on previous shell state\n\
                         - Work in a specific directory across multiple operations\n\
                         \
                         The session is per-conversation and automatically cleaned up when the conversation ends. \
                         \
                         For a command you expect to run long (a test suite, a data script, a build), pass \
                         `timeout_seconds` instead of wrapping the command in your own `timeout ... &` — a \
                         command that still times out returns whatever output it produced so far instead of \
                         discarding it."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute in the persistent shell session"
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": format!(
                        "Optional timeout for this command in seconds, overriding the configured default. \
                         Capped at {} seconds.",
                        MAX_SHELL_CALL_TIMEOUT_SECONDS
                    )
                }
            },
            "required": ["command"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
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

        tracing::debug!(
            command = %args.command,
            timeout_seconds = ?args.timeout_seconds,
            "Executing in shell session"
        );
        let output = self
            .session
            .execute_with_timeout(&args.command, args.timeout_seconds)
            .await?;
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

    fn description(&self) -> String {
        "Set an environment variable in the persistent shell session. \
                         The variable will be available to all subsequent commands in this session."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
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

    fn description(&self) -> String {
        "Change the working directory in the persistent shell session. \
                         The new directory will persist for all subsequent commands. \
                         If a workspace is configured, the path must stay within the workspace bounds."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to change to (absolute or relative)"
                }
            },
            "required": ["path"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
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

    fn description(&self) -> String {
        "Get the current status of the persistent shell session, including \
                         working directory, environment variables, process ID, and uptime."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn timeout_of(args: serde_json::Value) -> Option<u32> {
        serde_json::from_value::<ShellExecuteArgs>(args)
            .expect("shell_execute args parse")
            .timeout_seconds
    }

    #[test]
    fn timeout_seconds_is_read_leniently() {
        assert_eq!(timeout_of(serde_json::json!({"command": "ls"})), None);
        assert_eq!(
            timeout_of(serde_json::json!({"command": "ls", "timeout_seconds": 120})),
            Some(120)
        );
        assert_eq!(
            timeout_of(serde_json::json!({"command": "ls", "timeout_seconds": 300.0})),
            Some(300)
        );
        assert_eq!(
            timeout_of(serde_json::json!({"command": "ls", "timeout_seconds": "90"})),
            Some(90)
        );
        // Malformed values never fail the call; the default applies.
        for bad in [
            serde_json::json!(null),
            serde_json::json!("5m"),
            serde_json::json!(-3),
            serde_json::json!({"s": 1}),
        ] {
            assert_eq!(
                timeout_of(serde_json::json!({"command": "ls", "timeout_seconds": bad})),
                None
            );
        }
    }

    #[test]
    fn an_ordinary_result_does_not_mention_timeouts() {
        let output = ShellExecuteOutput::from(ShellOutput {
            stdout: "hi".into(),
            exit_code: 0,
            truncated: false,
            timed_out: false,
        });
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("timed_out").is_none(), "{json}");

        let timed_out = ShellExecuteOutput::from(ShellOutput {
            stdout: "partial".into(),
            exit_code: -1,
            truncated: false,
            timed_out: true,
        });
        let json = serde_json::to_value(&timed_out).unwrap();
        assert_eq!(json["timed_out"], serde_json::json!(true));
    }
}
