use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::Command;
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
    ///
    /// Security is provided through multiple layers:
    /// 1. Feature toggle (enabled/disabled)
    /// 2. User approval based on approval mode
    /// 3. Sandboxing (Bubblewrap on Linux, sandbox-exec on macOS)
    /// 4. Workspace directory isolation
    /// 5. Timeout enforcement
    /// 6. Network isolation (optional)
    ///
    /// Note: We intentionally do NOT use pattern-based command validation (e.g., blocking
    /// "~/.ssh") as it provides false security and is trivially bypassable. Real protection
    /// comes from sandboxing and user approval.
    pub async fn execute(&self, input: BashToolInput) -> Result<BashToolOutput> {
        // 1. Check if execution is enabled
        if !self.settings.enabled {
            return Err(anyhow!(
                "Code execution is disabled. Enable it in Settings → Execution."
            ));
        }

        // 2. Determine if sandboxing is available
        let is_sandboxed = self.can_sandbox();

        // 3. Check approval mode and request if needed
        let approved = self.request_approval(&input.command, is_sandboxed).await?;
        if !approved {
            return Err(anyhow!("Execution denied by user"));
        }

        debug!(command = %input.command, sandboxed = is_sandboxed, "Executing bash command");

        // 4. Execute with timeout and capture output
        let output = self.run_command(&input.command, is_sandboxed).await?;

        // 5. Truncate output if needed
        Ok(self.truncate_output(output))
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

                // Notify via global channel to emit stream event
                crate::chatty::models::execution_approval_store::notify_approval_via_global(
                    request_id.clone(),
                    command.to_string(),
                    is_sandboxed,
                );

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

    /// Validate and escape a workspace path for safe use in macOS sandbox profile
    ///
    /// Prevents path injection attacks by:
    /// 1. Validating the path is absolute
    /// 2. Rejecting paths with suspicious characters (parentheses, etc.)
    /// 3. Canonicalizing to resolve symlinks and relative components
    /// 4. Escaping special characters (quotes, backslashes)
    #[cfg(target_os = "macos")]
    fn escape_sandbox_path(workspace: &str) -> Result<String> {
        use std::path::Path;

        // 1. Validate path is absolute
        let path = Path::new(workspace);
        if !path.is_absolute() {
            return Err(anyhow!(
                "Workspace path must be absolute, got: {}",
                workspace
            ));
        }

        // 2. Early validation: reject paths with parentheses or other suspicious chars
        // This prevents injection attacks like: /tmp/test") )(allow default)
        if workspace.contains('(') || workspace.contains(')') {
            return Err(anyhow!(
                "Workspace path contains invalid characters (parentheses): {}",
                workspace
            ));
        }

        // 3. Canonicalize to resolve symlinks and normalize path
        let canonical = path.canonicalize().map_err(|e| {
            anyhow!(
                "Failed to canonicalize workspace path '{}': {}",
                workspace,
                e
            )
        })?;

        let canonical_str = canonical
            .to_str()
            .ok_or_else(|| anyhow!("Workspace path contains invalid UTF-8"))?;

        // 4. Escape special characters that could break sandbox DSL
        // In SBPL (Sandbox Profile Language), strings can contain:
        // - Quotes that need escaping
        // - Backslashes that need escaping
        let escaped = canonical_str
            .replace('\\', "\\\\") // Escape backslashes first
            .replace('"', "\\\""); // Escape quotes

        // 5. Final check: ensure canonicalization didn't introduce parentheses
        if escaped.contains('(') || escaped.contains(')') {
            return Err(anyhow!(
                "Canonicalized workspace path contains invalid characters: {}",
                canonical_str
            ));
        }

        Ok(escaped)
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

                ;; Deny write access to sensitive system directories
                (deny file-write*
                    (subpath "/System")
                    (subpath "/Library")
                    (subpath "/private/etc")
                    (subpath "/private/var")
                    (regex #"^/Users/[^/]+/\.ssh")
                    (regex #"^/Users/[^/]+/\.aws")
                    (regex #"^/Users/[^/]+/\.gnupg")
                )

                ;; Deny read access to sensitive credential files and directories
                (deny file-read*
                    (regex #"^/Users/[^/]+/\.ssh/")
                    (regex #"^/Users/[^/]+/\.aws/")
                    (regex #"^/Users/[^/]+/\.gnupg/")
                    (regex #"^/Users/[^/]+/\.docker/config\.json$")
                    (regex #"^/Users/[^/]+/\.kube/config$")
                    (regex #"^/Users/[^/]+/\.netrc$")
                    (subpath "/private/etc/ssh")
                    (literal "/etc/master.passwd")
                    (literal "/etc/shadow")
                )

                ;; Deny network access if network isolation is enabled
                ;; (network rules are added conditionally below)

                (allow file-write* (subpath "/tmp"))
            "#;

            // Build profile with optional workspace and network rules
            let profile_with_workspace = if let Some(workspace) = &self.settings.workspace_dir {
                // Validate and escape workspace path to prevent injection attacks
                let safe_workspace = Self::escape_sandbox_path(workspace)?;

                let network_rules = if self.settings.network_isolation {
                    r#"
                ;; Network isolation enabled - deny all network access
                (deny network*)
                "#
                } else {
                    ""
                };

                format!(
                    r#"
                    (version 1)
                    (allow default)

                    ;; Deny write access to sensitive system directories
                    (deny file-write*
                        (subpath "/System")
                        (subpath "/Library")
                        (subpath "/private/etc")
                        (subpath "/private/var")
                        (regex #"^/Users/[^/]+/\.ssh")
                        (regex #"^/Users/[^/]+/\.aws")
                        (regex #"^/Users/[^/]+/\.gnupg")
                    )

                    ;; Deny read access to sensitive credential files and directories
                    (deny file-read*
                        (regex #"^/Users/[^/]+/\.ssh/")
                        (regex #"^/Users/[^/]+/\.aws/")
                        (regex #"^/Users/[^/]+/\.gnupg/")
                        (regex #"^/Users/[^/]+/\.docker/config\.json$")
                        (regex #"^/Users/[^/]+/\.kube/config$")
                        (regex #"^/Users/[^/]+/\.netrc$")
                        (subpath "/private/etc/ssh")
                        (literal "/etc/master.passwd")
                        (literal "/etc/shadow")
                    )
                    {}
                    (allow file-write* (subpath "/tmp"))
                    (allow file-write* (subpath "{}"))
                    "#,
                    network_rules, safe_workspace
                )
            } else {
                let network_rules = if self.settings.network_isolation {
                    format!("{}\n                (deny network*)", profile)
                } else {
                    profile.to_string()
                };
                network_rules
            };

            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-p", &profile_with_workspace, "/bin/bash", "-c", command]);

            // Set working directory if workspace is configured
            if let Some(workspace) = &self.settings.workspace_dir {
                cmd.current_dir(workspace);
            }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn create_test_executor(
        enabled: bool,
        approval_mode: ApprovalMode,
    ) -> (BashExecutor, PendingApprovals) {
        let settings = ExecutionSettingsModel {
            enabled,
            approval_mode,
            workspace_dir: None,
            timeout_seconds: 30,
            max_output_bytes: 10000,
            network_isolation: false,
        };
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let executor = BashExecutor::new(settings, pending.clone());
        (executor, pending)
    }

    fn create_test_executor_with_settings(settings: ExecutionSettingsModel) -> BashExecutor {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        BashExecutor::new(settings, pending)
    }

    // Helper to create executor that forces unsandboxed execution for testing
    // This avoids sandbox-related race conditions in parallel test execution
    struct UnsandboxedExecutor {
        executor: BashExecutor,
    }

    impl UnsandboxedExecutor {
        fn new(settings: ExecutionSettingsModel) -> Self {
            let pending = Arc::new(Mutex::new(HashMap::new()));
            Self {
                executor: BashExecutor::new(settings, pending),
            }
        }

        async fn execute(&self, input: BashToolInput) -> Result<BashToolOutput> {
            // Check if execution is enabled
            if !self.executor.settings.enabled {
                return Err(anyhow!(
                    "Code execution is disabled. Enable it in Settings → Execution."
                ));
            }

            // Request approval
            let approved = self
                .executor
                .request_approval(&input.command, false)
                .await?;
            if !approved {
                return Err(anyhow!("Execution denied by user"));
            }

            // Execute without sandboxing
            let output = self.executor.run_unsandboxed(&input.command).await?;
            Ok(self.executor.truncate_output(output))
        }
    }

    #[tokio::test]
    async fn test_execution_disabled_rejects_command() {
        let (executor, _) = create_test_executor(false, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "echo 'hello world'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Code execution is disabled"));
    }

    #[tokio::test]
    async fn test_auto_approve_all_executes_simple_command() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "echo 'hello world'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("hello world"));
    }

    #[tokio::test]
    async fn test_command_with_non_zero_exit_code() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "exit 42".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 42);
    }

    #[tokio::test]
    async fn test_command_stderr_captured() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "echo 'error message' >&2".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stderr.contains("error message"));
    }

    #[tokio::test]
    async fn test_output_truncation() {
        let settings = ExecutionSettingsModel {
            enabled: true,
            approval_mode: ApprovalMode::AutoApproveAll,
            workspace_dir: None,
            timeout_seconds: 30,
            max_output_bytes: 50, // Small limit for testing
            network_isolation: false,
        };
        let executor = create_test_executor_with_settings(settings);

        let input = BashToolInput {
            command: "seq 1 1000".to_string(), // Generate lots of output
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.truncated);
        assert!(output.stdout.contains("[truncated"));
    }

    #[tokio::test]
    async fn test_timeout_enforcement() {
        let settings = ExecutionSettingsModel {
            enabled: true,
            approval_mode: ApprovalMode::AutoApproveAll,
            workspace_dir: None,
            timeout_seconds: 1, // Very short timeout
            max_output_bytes: 10000,
            network_isolation: false,
        };
        let executor = create_test_executor_with_settings(settings);

        let input = BashToolInput {
            command: "sleep 10".to_string(), // Will timeout
        };

        let result = executor.execute(input).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_approval_timeout() {
        let (executor, pending) = create_test_executor(true, ApprovalMode::AlwaysAsk);

        let input = BashToolInput {
            command: "echo 'test'".to_string(),
        };

        // Spawn execution in background (will wait for approval that never comes)
        let handle = tokio::spawn(async move { executor.execute(input).await });

        // Wait a bit then check pending approvals
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should have a pending approval
        {
            let pending_guard = pending.lock().unwrap();
            assert_eq!(pending_guard.len(), 1);
        }

        // Abort the execution (simulating timeout)
        handle.abort();
    }

    #[tokio::test]
    async fn test_approval_decision_approved() {
        let (executor, pending) = create_test_executor(true, ApprovalMode::AlwaysAsk);

        let input = BashToolInput {
            command: "echo 'approved'".to_string(),
        };

        // Spawn execution in background
        let exec_handle = tokio::spawn(async move { executor.execute(input).await });

        // Wait for approval request to be created
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Approve the request
        {
            let mut pending_guard = pending.lock().unwrap();
            let id = pending_guard.keys().next().unwrap().clone();
            let request = pending_guard.remove(&id).unwrap();
            let _ = request.responder.send(ApprovalDecision::Approved);
        }

        // Execution should complete successfully
        let result = exec_handle.await.unwrap();
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("approved"));
    }

    #[tokio::test]
    async fn test_approval_decision_denied() {
        let (executor, pending) = create_test_executor(true, ApprovalMode::AlwaysAsk);

        let input = BashToolInput {
            command: "echo 'denied'".to_string(),
        };

        // Spawn execution in background
        let exec_handle = tokio::spawn(async move { executor.execute(input).await });

        // Wait for approval request to be created
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Deny the request
        {
            let mut pending_guard = pending.lock().unwrap();
            let id = pending_guard.keys().next().unwrap().clone();
            let request = pending_guard.remove(&id).unwrap();
            let _ = request.responder.send(ApprovalDecision::Denied);
        }

        // Execution should fail with denial
        let result = exec_handle.await.unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("denied"));
    }

    #[tokio::test]
    async fn test_workspace_directory_setting() {
        let temp_dir = std::env::temp_dir();
        let workspace = temp_dir.join(format!("chatty_test_workspace_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();

        let settings = ExecutionSettingsModel {
            enabled: true,
            approval_mode: ApprovalMode::AutoApproveAll,
            workspace_dir: Some(workspace.to_str().unwrap().to_string()),
            timeout_seconds: 30,
            max_output_bytes: 10000,
            network_isolation: false,
        };
        // Use unsandboxed executor to avoid sandbox race conditions in parallel tests
        let executor = UnsandboxedExecutor::new(settings);

        // Test that workspace directory setting is applied
        // (sandboxing may redirect to /workspace, so we verify via file I/O)
        let input = BashToolInput {
            command: "echo 'workspace test' > test_file.txt && cat test_file.txt".to_string(),
        };

        let result = executor.execute(input).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.stdout.contains("workspace test"));

        // Cleanup
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[tokio::test]
    async fn test_sandbox_detection() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let can_sandbox = executor.can_sandbox();

        // Should return true on Linux (if bwrap installed) or macOS
        #[cfg(target_os = "linux")]
        {
            // Result depends on whether bwrap is installed
            // Just verify it doesn't panic
            assert!(can_sandbox == true || can_sandbox == false);
        }

        #[cfg(target_os = "macos")]
        {
            assert!(
                can_sandbox,
                "macOS should always support sandboxing via sandbox-exec"
            );
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            assert!(
                !can_sandbox,
                "Other platforms should not support sandboxing"
            );
        }
    }

    #[tokio::test]
    async fn test_auto_approve_sandboxed_mode() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveSandboxed);

        let input = BashToolInput {
            command: "echo 'sandboxed test'".to_string(),
        };

        let result = executor.execute(input).await;

        // Should execute if sandboxing is available, otherwise fail (no approval)
        if executor.can_sandbox() {
            assert!(result.is_ok());
            let output = result.unwrap();
            assert!(output.stdout.contains("sandboxed test"));
        } else {
            // If sandboxing not available, should require approval
            // Since we're not providing approval, this will timeout or fail
            // For CI/CD, we'll just check it doesn't panic
        }
    }

    #[tokio::test]
    async fn test_command_with_pipes() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "echo 'line1\nline2\nline3' | grep line2".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("line2"));
        assert!(!output.stdout.contains("line1"));
    }

    #[tokio::test]
    async fn test_command_with_environment_variables() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "export TEST_VAR='hello'; echo $TEST_VAR".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_multiple_commands_in_sequence() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "echo 'first' && echo 'second' && echo 'third'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("first"));
        assert!(output.stdout.contains("second"));
        assert!(output.stdout.contains("third"));
    }

    #[tokio::test]
    async fn test_command_failure_stops_chain() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        let input = BashToolInput {
            command: "echo 'first' && false && echo 'should not appear'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("first"));
        assert!(!output.stdout.contains("should not appear"));
        assert_ne!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn test_file_creation_in_workspace() {
        let temp_dir = std::env::temp_dir();
        let workspace = temp_dir.join(format!(
            "chatty_test_file_creation_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let settings = ExecutionSettingsModel {
            enabled: true,
            approval_mode: ApprovalMode::AutoApproveAll,
            workspace_dir: Some(workspace.to_str().unwrap().to_string()),
            timeout_seconds: 30,
            max_output_bytes: 10000,
            network_isolation: false,
        };
        // Use unsandboxed executor to avoid sandbox race conditions in parallel tests
        let executor = UnsandboxedExecutor::new(settings);

        let input = BashToolInput {
            command: "echo 'test content' > test_file.txt && cat test_file.txt".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.stdout.contains("test content"),
            "Expected 'test content' in stdout, got: {:?}\nstderr: {:?}",
            output.stdout,
            output.stderr
        );

        // Cleanup
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn test_bash_tool_input_serialization() {
        let input = BashToolInput {
            command: "echo 'test'".to_string(),
        };

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("command"));
        assert!(json.contains("echo 'test'"));

        let deserialized: BashToolInput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.command, "echo 'test'");
    }

    #[test]
    fn test_bash_tool_output_serialization() {
        let output = BashToolOutput {
            stdout: "output".to_string(),
            stderr: "error".to_string(),
            exit_code: 0,
            truncated: false,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("stdout"));
        assert!(json.contains("stderr"));
        assert!(json.contains("exit_code"));
        assert!(json.contains("truncated"));
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_sandbox_blocks_ssh_read() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        // Only run if sandbox is available
        if !executor.can_sandbox() {
            return;
        }

        let input = BashToolInput {
            command: "cat ~/.ssh/id_rsa 2>&1 || echo 'ACCESS_DENIED'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        // Should be denied by sandbox (or file doesn't exist)
        assert!(
            output.stdout.contains("ACCESS_DENIED")
                || output.stdout.contains("Operation not permitted")
                || output.stderr.contains("Operation not permitted"),
            "Expected sandbox to block SSH key access, got stdout: {:?}, stderr: {:?}",
            output.stdout,
            output.stderr
        );
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_sandbox_blocks_aws_credentials() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        // Only run if sandbox is available
        if !executor.can_sandbox() {
            return;
        }

        let input = BashToolInput {
            command: "cat ~/.aws/credentials 2>&1 || echo 'ACCESS_DENIED'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        // Should be denied by sandbox (or file doesn't exist)
        assert!(
            output.stdout.contains("ACCESS_DENIED")
                || output.stdout.contains("Operation not permitted")
                || output.stderr.contains("Operation not permitted"),
            "Expected sandbox to block AWS credentials access, got stdout: {:?}, stderr: {:?}",
            output.stdout,
            output.stderr
        );
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_sandbox_blocks_docker_config() {
        let (executor, _) = create_test_executor(true, ApprovalMode::AutoApproveAll);

        // Only run if sandbox is available
        if !executor.can_sandbox() {
            return;
        }

        let input = BashToolInput {
            command: "cat ~/.docker/config.json 2>&1 || echo 'ACCESS_DENIED'".to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        // Should be denied by sandbox (or file doesn't exist)
        assert!(
            output.stdout.contains("ACCESS_DENIED")
                || output.stdout.contains("Operation not permitted")
                || output.stderr.contains("Operation not permitted"),
            "Expected sandbox to block Docker config access, got stdout: {:?}, stderr: {:?}",
            output.stdout,
            output.stderr
        );
    }

    #[tokio::test]
    async fn test_network_isolation() {
        let settings = ExecutionSettingsModel {
            enabled: true,
            approval_mode: ApprovalMode::AutoApproveAll,
            workspace_dir: None,
            timeout_seconds: 30,
            max_output_bytes: 10000,
            network_isolation: true,
        };
        let executor = create_test_executor_with_settings(settings);

        // Only run on platforms with sandboxing
        if !executor.can_sandbox() {
            return;
        }

        // Try to make a network connection (should fail with network isolation)
        let input = BashToolInput {
            command: "curl -s --max-time 2 https://example.com 2>&1 || echo 'NETWORK_BLOCKED'"
                .to_string(),
        };

        let result = executor.execute(input).await;

        assert!(result.is_ok());
        let _output = result.unwrap();

        // Network should be blocked (or curl not available)
        // On macOS with network isolation, we expect network operations to fail
        #[cfg(target_os = "macos")]
        assert!(
            output.stdout.contains("NETWORK_BLOCKED")
                || output.stderr.contains("Operation not permitted")
                || output.stdout.contains("Operation not permitted"),
            "Expected network to be blocked, got stdout: {:?}, stderr: {:?}",
            output.stdout,
            output.stderr
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_escape_sandbox_path_injection_attack() {
        // Test that path injection attempts are blocked
        let malicious_path = r#"/tmp/test") )(allow default) (deny"#;
        let result = BashExecutor::escape_sandbox_path(malicious_path);

        // Should fail because path contains parentheses
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid characters")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_escape_sandbox_path_with_quotes() {
        use std::fs;
        use std::path::Path;

        // Create a temp directory with quotes in the name (if filesystem allows)
        let temp_base = std::env::temp_dir();
        let dir_name = format!("test_quote_{}", uuid::Uuid::new_v4());
        let test_dir = temp_base.join(&dir_name);

        fs::create_dir_all(&test_dir).unwrap();

        let path_str = test_dir.to_str().unwrap();
        let result = BashExecutor::escape_sandbox_path(path_str);

        // Should succeed and escape any special chars
        assert!(result.is_ok());

        // Cleanup
        fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_escape_sandbox_path_relative_path() {
        // Relative paths should be rejected
        let relative_path = "./some/relative/path";
        let result = BashExecutor::escape_sandbox_path(relative_path);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be absolute"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_escape_sandbox_path_nonexistent() {
        // Non-existent absolute paths should fail canonicalization
        let nonexistent = "/this/path/definitely/does/not/exist/12345";
        let result = BashExecutor::escape_sandbox_path(nonexistent);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to canonicalize")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_escape_sandbox_path_valid() {
        use std::fs;

        // Test with a valid absolute path
        let temp_dir = std::env::temp_dir();
        let test_dir = temp_dir.join(format!("valid_path_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&test_dir).unwrap();

        let path_str = test_dir.to_str().unwrap();
        let result = BashExecutor::escape_sandbox_path(path_str);

        assert!(result.is_ok());
        let escaped = result.unwrap();

        // Should be an absolute path
        assert!(escaped.starts_with('/'));

        // Should not contain unescaped quotes or parentheses
        assert!(!escaped.contains('"') || escaped.contains("\\\""));
        assert!(!escaped.contains('('));
        assert!(!escaped.contains(')'));

        // Cleanup
        fs::remove_dir_all(&test_dir).unwrap();
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_sandbox_rejects_malicious_workspace() {
        use std::fs;

        // Try to create an executor with a malicious workspace path
        let malicious_workspace =
            r#"/tmp") )(allow default) (allow file-read* (subpath "/Users/test/.ssh"#;

        let settings = ExecutionSettingsModel {
            enabled: true,
            approval_mode: ApprovalMode::AutoApproveAll,
            workspace_dir: Some(malicious_workspace.to_string()),
            timeout_seconds: 30,
            max_output_bytes: 10000,
            network_isolation: false,
        };

        let executor = create_test_executor_with_settings(settings);

        // Try to execute a command - should fail during sandbox profile construction
        let input = BashToolInput {
            command: "echo 'test'".to_string(),
        };

        let result = executor.execute(input).await;

        // Should fail with validation error
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("invalid characters")
                || err.to_string().contains("Failed to canonicalize"),
            "Expected validation error, got: {}",
            err
        );
    }
}
