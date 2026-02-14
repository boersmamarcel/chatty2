use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::chatty::models::execution_approval_store::{
    ApprovalDecision, ExecutionApprovalRequest, PendingApprovals,
};
use crate::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};

/// Input for bash tool execution
#[derive(Debug, Deserialize, Serialize)]
pub struct BashToolInput {
    pub command: String,
}

/// Output from bash tool execution
#[derive(Debug, Serialize)]
pub struct BashToolOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub truncated: bool,
}

/// Core executor for bash commands with security features
pub struct BashExecutor {
    settings: ExecutionSettingsModel,
    pending_approvals: PendingApprovals,
}

impl BashExecutor {
    pub fn new(settings: ExecutionSettingsModel, pending_approvals: PendingApprovals) -> Self {
        Self {
            settings,
            pending_approvals,
        }
    }

    /// Execute a bash command with approval, validation, and sandboxing
    pub async fn execute(&self, input: BashToolInput) -> Result<BashToolOutput> {
        // 1. Check if execution is enabled
        if !self.settings.enabled {
            return Err(anyhow!(
                "Code execution is disabled. Enable it in Settings → Execution."
            ));
        }

        // 2. Validate command (block sensitive paths)
        self.validate_command(&input.command)?;

        // 3. Determine if sandboxing is available
        let is_sandboxed = self.can_sandbox();

        // 4. Check approval mode and request if needed
        let approved = self.request_approval(&input.command, is_sandboxed).await?;
        if !approved {
            return Err(anyhow!("Execution denied by user"));
        }

        debug!(command = %input.command, sandboxed = is_sandboxed, "Executing bash command");

        // 5. Execute with timeout and capture output
        let output = self.run_command(&input.command, is_sandboxed).await?;

        // 6. Truncate output if needed
        Ok(self.truncate_output(output))
    }

    /// Validate command to block dangerous patterns
    fn validate_command(&self, command: &str) -> Result<()> {
        let blocked_patterns = [
            "~/.ssh",
            "~/.aws",
            "~/.gnupg",
            "/etc/passwd",
            "/etc/shadow",
            "rm -rf /",
            "rm -rf ~",
        ];

        for pattern in &blocked_patterns {
            if command.contains(pattern) {
                return Err(anyhow!(
                    "Command blocked: contains dangerous pattern '{}'. \
                     This pattern is blocked to protect sensitive files.",
                    pattern
                ));
            }
        }

        Ok(())
    }

    /// Check if sandboxing is available on this platform
    fn can_sandbox(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            // Check for bubblewrap availability
            Command::new("bwrap")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        #[cfg(target_os = "macos")]
        {
            // macOS has sandbox-exec built-in
            true
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }

    /// Request approval from user based on approval mode
    async fn request_approval(&self, command: &str, is_sandboxed: bool) -> Result<bool> {
        match self.settings.approval_mode {
            ApprovalMode::AutoApproveAll => {
                debug!("Auto-approving command (AutoApproveAll mode)");
                Ok(true)
            }
            ApprovalMode::AutoApproveSandboxed if is_sandboxed => {
                debug!("Auto-approving sandboxed command (AutoApproveSandboxed mode)");
                Ok(true)
            }
            ApprovalMode::AlwaysAsk | ApprovalMode::AutoApproveSandboxed => {
                // Create oneshot channel for response
                let (tx, rx) = tokio::sync::oneshot::channel();
                let request_id = uuid::Uuid::new_v4().to_string();

                debug!(
                    request_id = %request_id,
                    "Requesting user approval for command execution"
                );

                // Create approval request
                let request = ExecutionApprovalRequest {
                    id: request_id.clone(),
                    command: command.to_string(),
                    is_sandboxed,
                    created_at: SystemTime::now(),
                    responder: tx,
                };

                // Add to pending approvals store
                {
                    let mut pending = self.pending_approvals.lock().unwrap();
                    pending.insert(request_id.clone(), request);
                }

                // TODO: Emit event to UI to show approval prompt
                // This will be handled by the stream processor

                // Wait for approval with 5 minute timeout
                match timeout(Duration::from_secs(300), rx).await {
                    Ok(Ok(ApprovalDecision::Approved)) => {
                        debug!(request_id = %request_id, "Command execution approved by user");
                        Ok(true)
                    }
                    Ok(Ok(ApprovalDecision::Denied)) => {
                        debug!(request_id = %request_id, "Command execution denied by user");
                        Ok(false)
                    }
                    Ok(Err(_)) => {
                        warn!(request_id = %request_id, "Approval channel closed unexpectedly");
                        Err(anyhow!("Approval channel closed"))
                    }
                    Err(_) => {
                        warn!(request_id = %request_id, "Approval request timed out after 5 minutes");
                        // Clean up pending request
                        let mut pending = self.pending_approvals.lock().unwrap();
                        pending.remove(&request_id);
                        Err(anyhow!("Approval timeout (5 minutes)"))
                    }
                }
            }
        }
    }

    /// Execute command with timeout
    async fn run_command(&self, command: &str, is_sandboxed: bool) -> Result<std::process::Output> {
        let timeout_duration = Duration::from_secs(self.settings.timeout_seconds as u64);

        let result = if is_sandboxed {
            timeout(timeout_duration, self.run_sandboxed(command)).await
        } else {
            timeout(timeout_duration, self.run_unsandboxed(command)).await
        };

        result.map_err(|_| {
            anyhow!(
                "Command execution timed out after {} seconds",
                self.settings.timeout_seconds
            )
        })?
    }

    /// Execute command in sandbox (Linux: Bubblewrap, macOS: sandbox-exec)
    async fn run_sandboxed(&self, command: &str) -> Result<std::process::Output> {
        #[cfg(target_os = "linux")]
        {
            let mut cmd = Command::new("bwrap");

            // Bind essential system directories as read-only
            cmd.args([
                "--ro-bind",
                "/usr",
                "/usr",
                "--ro-bind",
                "/lib",
                "/lib",
                "--ro-bind",
                "/bin",
                "/bin",
                "--ro-bind",
                "/sbin",
                "/sbin",
                "--tmpfs",
                "/tmp",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--unshare-all",
                "--die-with-parent",
            ]);

            // Check for /lib64 (exists on many 64-bit Linux systems)
            if std::path::Path::new("/lib64").exists() {
                cmd.args(["--ro-bind", "/lib64", "/lib64"]);
            }

            // Network isolation if enabled
            if self.settings.network_isolation {
                cmd.arg("--unshare-net");
            }

            // Add workspace directory if configured
            if let Some(workspace) = &self.settings.workspace_dir {
                cmd.args(["--bind", workspace, "/workspace"]);
                cmd.args(["--chdir", "/workspace"]);
            }

            cmd.args(["/bin/bash", "-c", command]);

            tokio::process::Command::from(cmd)
                .output()
                .await
                .map_err(|e| anyhow!("Failed to execute sandboxed command: {}", e))
        }

        #[cfg(target_os = "macos")]
        {
            // macOS sandbox profile - permissive with specific denials
            // Using (allow default) instead of (deny default) to prevent fork() blocking
            let profile = r#"
                (version 1)
                (allow default)
                (deny file-write*
                    (subpath "/System")
                    (subpath "/Library")
                    (subpath "/private/etc")
                    (subpath "/private/var")
                    (regex #"^/Users/[^/]+/\.ssh")
                    (regex #"^/Users/[^/]+/\.aws")
                    (regex #"^/Users/[^/]+/\.gnupg")
                )
                (allow file-write* (subpath "/tmp"))
            "#;

            // Add workspace directory write permissions if configured
            let profile_with_workspace = if let Some(workspace) = &self.settings.workspace_dir {
                format!(
                    r#"
                    (version 1)
                    (allow default)
                    (deny file-write*
                        (subpath "/System")
                        (subpath "/Library")
                        (subpath "/private/etc")
                        (subpath "/private/var")
                        (regex #"^/Users/[^/]+/\.ssh")
                        (regex #"^/Users/[^/]+/\.aws")
                        (regex #"^/Users/[^/]+/\.gnupg")
                    )
                    (allow file-write* (subpath "/tmp"))
                    (allow file-write* (subpath "{}"))
                    "#,
                    workspace
                )
            } else {
                profile.to_string()
            };

            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-p", &profile_with_workspace, "/bin/bash", "-c", command]);

            tokio::process::Command::from(cmd)
                .output()
                .await
                .map_err(|e| anyhow!("Failed to execute sandboxed command: {}", e))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(anyhow!(
                "Sandboxing not supported on this platform. \
                 Set approval mode to allow unsandboxed execution."
            ))
        }
    }

    /// Execute command without sandboxing (requires approval)
    async fn run_unsandboxed(&self, command: &str) -> Result<std::process::Output> {
        let mut cmd = Command::new("/bin/bash");
        cmd.args(["-c", command]);

        // Set working directory if configured
        if let Some(workspace) = &self.settings.workspace_dir {
            cmd.current_dir(workspace);
        }

        tokio::process::Command::from(cmd)
            .output()
            .await
            .map_err(|e| anyhow!("Failed to execute command: {}", e))
    }

    /// Truncate output to max size limit
    fn truncate_output(&self, output: std::process::Output) -> BashToolOutput {
        let max_bytes = self.settings.max_output_bytes;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let stdout_truncated = stdout.len() > max_bytes;
        let stderr_truncated = stderr.len() > max_bytes;
        let truncated = stdout_truncated || stderr_truncated;

        let stdout_final = if stdout_truncated {
            format!(
                "{}... [truncated {} bytes]",
                &stdout[..max_bytes],
                stdout.len() - max_bytes
            )
        } else {
            stdout.to_string()
        };

        let stderr_final = if stderr_truncated {
            format!(
                "{}... [truncated {} bytes]",
                &stderr[..max_bytes],
                stderr.len() - max_bytes
            )
        } else {
            stderr.to_string()
        };

        BashToolOutput {
            stdout: stdout_final,
            stderr: stderr_final,
            exit_code: output.status.code().unwrap_or(-1),
            truncated,
        }
    }
}
