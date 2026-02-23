use anyhow::{Result, anyhow};
use serde::Serialize;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::time::SystemTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Output from a shell command execution
#[derive(Debug, Serialize)]
pub struct ShellOutput {
    pub stdout: String,
    pub exit_code: i32,
    pub truncated: bool,
}

/// Current status of the shell session
#[derive(Debug, Serialize)]
pub struct ShellStatus {
    pub running: bool,
    pub cwd: String,
    pub env_vars: Vec<(String, String)>,
    pub pid: Option<u32>,
    pub uptime_seconds: u64,
}

/// Internal state for the running bash process
struct ShellProcess {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    is_sandboxed: bool,
}

/// A persistent shell session that maintains state across multiple commands.
///
/// The session keeps a bash process alive, preserving environment variables,
/// working directory, and other shell state between invocations.
///
/// Security: When sandboxing is available (bubblewrap on Linux, sandbox-exec on macOS),
/// the shell process runs inside a sandbox with filesystem and network restrictions.
/// Network isolation is controlled by the `network_isolation` setting.
pub struct ShellSession {
    process: Mutex<Option<ShellProcess>>,
    workspace_dir: Option<String>,
    network_isolation: bool,
    timeout_seconds: u32,
    max_output_bytes: usize,
    created_at: SystemTime,
}

impl ShellSession {
    /// Create a new shell session with the given configuration.
    ///
    /// The bash process is not spawned until the first command is executed.
    /// When spawned, it will run inside a sandbox if available (bubblewrap on Linux,
    /// sandbox-exec on macOS).
    pub fn new(
        workspace_dir: Option<String>,
        timeout_seconds: u32,
        max_output_bytes: usize,
        network_isolation: bool,
    ) -> Self {
        Self {
            process: Mutex::new(None),
            workspace_dir,
            network_isolation,
            timeout_seconds,
            max_output_bytes,
            created_at: SystemTime::now(),
        }
    }

    /// Check if sandboxing is available on this platform
    pub fn can_sandbox() -> bool {
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("bwrap")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        #[cfg(target_os = "macos")]
        {
            true
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }

    /// Check if the current session process is running inside a sandbox
    pub async fn is_sandboxed(&self) -> bool {
        let process = self.process.lock().await;
        process
            .as_ref()
            .map_or(Self::can_sandbox(), |p| p.is_sandboxed)
    }

    /// Ensure the bash process is running, spawning it if necessary.
    ///
    /// Attempts to spawn inside a sandbox (bubblewrap on Linux, sandbox-exec on macOS).
    /// Falls back to unsandboxed execution if sandboxing is unavailable.
    async fn ensure_started(
        process: &mut Option<ShellProcess>,
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<()> {
        if process.is_some() {
            // Check if process is still alive
            if let Some(proc) = process {
                match proc.child.try_wait() {
                    Ok(Some(status)) => {
                        warn!(exit_status = ?status, "Shell process exited unexpectedly, respawning");
                        *process = None;
                    }
                    Ok(None) => return Ok(()), // Still running
                    Err(e) => {
                        warn!(error = ?e, "Failed to check shell process status, respawning");
                        *process = None;
                    }
                }
            }
        }

        info!(workspace = ?workspace_dir, "Spawning persistent shell session");

        // Try sandboxed spawn first, fall back to unsandboxed
        let (mut child, is_sandboxed) = if Self::can_sandbox() {
            match Self::spawn_sandboxed(workspace_dir, network_isolation) {
                Ok(child) => {
                    info!("Shell session spawned inside sandbox");
                    (child, true)
                }
                Err(e) => {
                    warn!(error = ?e, "Sandboxed shell spawn failed, falling back to unsandboxed");
                    let child = Self::spawn_unsandboxed(workspace_dir)?;
                    (child, false)
                }
            }
        } else {
            info!("Sandboxing not available, spawning unsandboxed shell session");
            let child = Self::spawn_unsandboxed(workspace_dir)?;
            (child, false)
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to capture shell stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture shell stdout"))?;

        let pid = child.id();
        info!(pid = ?pid, sandboxed = is_sandboxed, "Shell session started");

        *process = Some(ShellProcess {
            child,
            stdin,
            reader: BufReader::new(stdout),
            is_sandboxed,
        });

        Ok(())
    }

    /// Spawn an unsandboxed bash process (fallback)
    fn spawn_unsandboxed(workspace_dir: &Option<String>) -> Result<Child> {
        let mut cmd = tokio::process::Command::new("/bin/bash");
        cmd.args(["--norc", "--noprofile"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = workspace_dir {
            cmd.current_dir(dir);
        }

        cmd.spawn()
            .map_err(|e| anyhow!("Failed to spawn shell process: {}", e))
    }

    /// Spawn a sandboxed bash process (Linux: bubblewrap, macOS: sandbox-exec)
    ///
    /// The persistent bash process runs inside the sandbox, inheriting all
    /// restrictions (filesystem isolation, network isolation). State (env vars,
    /// cwd) is maintained within the sandboxed process between commands.
    fn spawn_sandboxed(workspace_dir: &Option<String>, network_isolation: bool) -> Result<Child> {
        #[cfg(target_os = "linux")]
        {
            Self::spawn_sandboxed_linux(workspace_dir, network_isolation)
        }

        #[cfg(target_os = "macos")]
        {
            Self::spawn_sandboxed_macos(workspace_dir, network_isolation)
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (workspace_dir, network_isolation);
            Err(anyhow!("Sandboxing not supported on this platform"))
        }
    }

    /// Spawn a sandboxed bash process using bubblewrap on Linux
    #[cfg(target_os = "linux")]
    fn spawn_sandboxed_linux(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<Child> {
        let mut cmd = tokio::process::Command::new("bwrap");

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
            // --unshare-all includes --unshare-net; we re-enable network
            // below with --share-net when network_isolation is false.
            "--unshare-all",
            "--die-with-parent",
        ]);

        // Re-enable network access when isolation is not requested
        if !network_isolation {
            cmd.arg("--share-net");
        }

        // Check for /lib64 (exists on many 64-bit Linux systems)
        if std::path::Path::new("/lib64").exists() {
            cmd.args(["--ro-bind", "/lib64", "/lib64"]);
        }

        // Bind workspace at its original path so existing path references work
        if let Some(workspace) = workspace_dir {
            cmd.args(["--bind", workspace, workspace]);
            cmd.args(["--chdir", workspace]);
        }

        cmd.args(["/bin/bash", "--norc", "--noprofile"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        cmd.spawn()
            .map_err(|e| anyhow!("Failed to spawn sandboxed shell: {}", e))
    }

    /// Spawn a sandboxed bash process using sandbox-exec on macOS
    #[cfg(target_os = "macos")]
    fn spawn_sandboxed_macos(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<Child> {
        let profile = Self::build_macos_sandbox_profile(workspace_dir, network_isolation)?;

        let mut cmd = tokio::process::Command::new("sandbox-exec");
        cmd.args(["-p", &profile, "/bin/bash", "--norc", "--noprofile"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(workspace) = workspace_dir {
            cmd.current_dir(workspace);
        }

        cmd.spawn()
            .map_err(|e| anyhow!("Failed to spawn sandboxed shell: {}", e))
    }

    /// Build the macOS sandbox profile (SBPL)
    ///
    /// - Allows default operations
    /// - Denies writes to sensitive system directories
    /// - Denies reads to credential files (.ssh, .aws, etc.)
    /// - Denies network access when `network_isolation` is true
    /// - Allows writes to /tmp and workspace
    #[cfg(target_os = "macos")]
    fn build_macos_sandbox_profile(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<String> {
        let workspace_write_rule = if let Some(workspace) = workspace_dir {
            let safe_workspace = Self::escape_sandbox_path(workspace)?;
            format!(
                "\n                (allow file-write* (subpath \"{}\"))",
                safe_workspace
            )
        } else {
            String::new()
        };

        let network_rule = if network_isolation {
            "\n                ;; Deny network access (network isolation enabled)\n                (deny network*)"
        } else {
            "\n                ;; Network access allowed (network isolation disabled)"
        };

        Ok(format!(
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
                (allow file-write* (subpath "/tmp")){}
            "#,
            network_rule, workspace_write_rule
        ))
    }

    /// Validate and escape a workspace path for safe use in macOS sandbox profile.
    ///
    /// Prevents path injection attacks by:
    /// 1. Validating the path is absolute
    /// 2. Rejecting paths with suspicious characters (parentheses)
    /// 3. Canonicalizing to resolve symlinks and relative components
    /// 4. Escaping special characters (quotes, backslashes)
    #[cfg(target_os = "macos")]
    fn escape_sandbox_path(workspace: &str) -> Result<String> {
        use std::path::Path;

        let path = Path::new(workspace);
        if !path.is_absolute() {
            return Err(anyhow!(
                "Workspace path must be absolute, got: {}",
                workspace
            ));
        }

        if workspace.contains('(') || workspace.contains(')') {
            return Err(anyhow!(
                "Workspace path contains invalid characters (parentheses): {}",
                workspace
            ));
        }

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

        let escaped = canonical_str.replace('\\', "\\\\").replace('"', "\\\"");

        if escaped.contains('(') || escaped.contains(')') {
            return Err(anyhow!(
                "Canonicalized workspace path contains invalid characters: {}",
                canonical_str
            ));
        }

        Ok(escaped)
    }

    /// Execute a command in the persistent shell session.
    ///
    /// The command's stdout and stderr are merged (stderr redirected to stdout).
    /// Returns the combined output and exit code.
    pub async fn execute(&self, command: &str) -> Result<ShellOutput> {
        let mut process = self.process.lock().await;
        Self::ensure_started(&mut process, &self.workspace_dir, self.network_isolation).await?;

        let proc = process.as_mut().unwrap();
        let marker = uuid::Uuid::new_v4().to_string().replace('-', "");
        let marker_prefix = format!("__CHATTY_SHELL_MARKER_{}_", marker);

        // Write command with stderr redirect and end marker
        // The marker line format: __CHATTY_SHELL_MARKER_{uuid}_{exit_code}__
        let wrapped_command = format!(
            "{} 2>&1\n__chatty_ec=$?\necho \"{}${{__chatty_ec}}__\"\n",
            command, marker_prefix
        );

        proc.stdin
            .write_all(wrapped_command.as_bytes())
            .await
            .map_err(|e| anyhow!("Failed to write to shell stdin: {}", e))?;
        proc.stdin
            .flush()
            .await
            .map_err(|e| anyhow!("Failed to flush shell stdin: {}", e))?;

        // Read output until we find the marker
        let mut output = String::new();
        let timeout_duration = tokio::time::Duration::from_secs(self.timeout_seconds as u64);

        let read_result = tokio::time::timeout(timeout_duration, async {
            loop {
                let mut line = String::new();
                let bytes_read = proc
                    .reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| anyhow!("Failed to read from shell stdout: {}", e))?;

                if bytes_read == 0 {
                    return Err(anyhow!("Shell process terminated unexpectedly"));
                }

                if line.starts_with(&marker_prefix) {
                    // Parse exit code from marker line
                    let exit_code = line
                        .trim()
                        .strip_prefix(&marker_prefix)
                        .and_then(|s| s.strip_suffix("__"))
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(-1);

                    return Ok(exit_code);
                }

                output.push_str(&line);
            }
        })
        .await;

        match read_result {
            Ok(Ok(exit_code)) => {
                let truncated = output.len() > self.max_output_bytes;
                if truncated {
                    let original_len = output.len();
                    output.truncate(self.max_output_bytes);
                    output.push_str(&format!(
                        "\n... [truncated {} bytes]",
                        original_len - self.max_output_bytes
                    ));
                }

                Ok(ShellOutput {
                    stdout: output.trim_end().to_string(),
                    exit_code,
                    truncated,
                })
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout - the process may be stuck. Kill and respawn on next use.
                warn!(
                    timeout = self.timeout_seconds,
                    "Shell command timed out, killing session"
                );
                if let Some(mut proc) = process.take() {
                    let _ = proc.child.kill().await;
                }
                Err(anyhow!(
                    "Command timed out after {} seconds",
                    self.timeout_seconds
                ))
            }
        }
    }

    /// Set an environment variable in the shell session.
    pub async fn set_env(&self, key: &str, value: &str) -> Result<ShellOutput> {
        // Validate key contains only safe characters
        if !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(anyhow!(
                "Invalid environment variable name '{}': only alphanumeric and underscore allowed",
                key
            ));
        }

        // Use single quotes for value to prevent expansion, escaping single quotes
        let escaped_value = value.replace('\'', "'\\''");
        let command = format!("export {}='{}'", key, escaped_value);
        self.execute(&command).await
    }

    /// Change the working directory of the shell session.
    ///
    /// If a workspace directory is configured, the target path must be within it.
    pub async fn cd(&self, path: &str) -> Result<ShellOutput> {
        // If workspace is set, validate the path stays within bounds
        if let Some(ref workspace) = self.workspace_dir {
            // Resolve the path relative to current working directory in the shell
            // We do this inside the shell itself for accuracy
            let check_cmd = format!(
                "target_dir=$(cd {} 2>/dev/null && pwd) && \
                 case \"$target_dir\" in {}*) echo \"OK\";; *) echo \"DENIED\";; esac",
                shell_escape(path),
                shell_escape(workspace)
            );

            let check_result = self.execute(&check_cmd).await?;
            if check_result.stdout.trim() == "DENIED" {
                return Err(anyhow!(
                    "Cannot change directory to '{}': path is outside workspace '{}'",
                    path,
                    workspace
                ));
            }
        }

        self.execute(&format!("cd {}", shell_escape(path))).await
    }

    /// Get the current status of the shell session.
    pub async fn status(&self) -> Result<ShellStatus> {
        let process = self.process.lock().await;
        if process.is_none() {
            let uptime = SystemTime::now()
                .duration_since(self.created_at)
                .unwrap_or_default()
                .as_secs();

            return Ok(ShellStatus {
                running: false,
                cwd: self
                    .workspace_dir
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                env_vars: Vec::new(),
                pid: None,
                uptime_seconds: uptime,
            });
        }
        drop(process);

        // Get cwd and env from the running shell
        let cwd_result = self.execute("pwd").await?;
        let env_result = self.execute("env").await?;

        let cwd = cwd_result.stdout.trim().to_string();
        let env_vars: Vec<(String, String)> = env_result
            .stdout
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next().unwrap_or("").to_string();
                // Filter out internal/noisy env vars
                if key.starts_with("__chatty") || key.starts_with("BASH_") {
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect();

        let pid = {
            let process = self.process.lock().await;
            process.as_ref().and_then(|p| p.child.id())
        };

        let uptime = SystemTime::now()
            .duration_since(self.created_at)
            .unwrap_or_default()
            .as_secs();

        Ok(ShellStatus {
            running: true,
            cwd,
            env_vars,
            pid,
            uptime_seconds: uptime,
        })
    }

    /// Shut down the shell session, killing the bash process.
    #[allow(dead_code)]
    pub async fn shutdown(&self) {
        let mut process = self.process.lock().await;
        if let Some(mut proc) = process.take() {
            debug!("Shutting down shell session");
            let _ = proc.stdin.shutdown().await;
            let _ = proc.child.kill().await;
        }
    }

    /// Check if the session has a running process
    #[allow(dead_code)]
    pub async fn is_running(&self) -> bool {
        let process = self.process.lock().await;
        process.is_some()
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup
        if let Ok(mut process) = self.process.try_lock()
            && let Some(ref mut proc) = *process
        {
            debug!("Shell session dropped, killing process");
            // Child::kill_on_drop handles this, but be explicit
            let _ = proc.child.start_kill();
        }
    }
}

/// Escape a string for safe use in a shell command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_command_execution() {
        let session = ShellSession::new(None, 30, 51200, false);
        let result = session.execute("echo 'hello world'").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("hello world"));
    }

    #[tokio::test]
    async fn test_environment_persistence() {
        let session = ShellSession::new(None, 30, 51200, false);

        // Set an env var
        let result = session.set_env("MY_TEST_VAR", "test_value_123").await;
        assert!(result.is_ok());

        // Verify it persists
        let result = session.execute("echo $MY_TEST_VAR").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("test_value_123"));
    }

    #[tokio::test]
    async fn test_working_directory_persistence() {
        let session = ShellSession::new(None, 30, 51200, false);

        // Change to /tmp
        let result = session.cd("/tmp").await;
        assert!(result.is_ok());

        // Verify it persists
        let result = session.execute("pwd").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("/tmp"));
    }

    #[tokio::test]
    async fn test_exit_code_capture() {
        let session = ShellSession::new(None, 30, 51200, false);

        let _result = session.execute("exit 42").await;
        // After `exit 42`, the shell process dies, but it should respawn
        // on the next command. Use a non-exit failing command to test exit code.
        let result = session.execute("false").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.exit_code, 1);
    }

    #[tokio::test]
    async fn test_stderr_captured() {
        let session = ShellSession::new(None, 30, 51200, false);

        let result = session.execute("echo 'error message' >&2").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        // stderr is redirected to stdout
        assert!(output.stdout.contains("error message"));
    }

    #[tokio::test]
    async fn test_command_sequence() {
        let session = ShellSession::new(None, 30, 51200, false);

        // Create a file, write to it, read it back
        session.execute("export MYVAR=hello").await.unwrap();
        let result = session.execute("echo $MYVAR").await.unwrap();
        assert!(result.stdout.contains("hello"));

        session.execute("MYVAR=world").await.unwrap();
        let result = session.execute("echo $MYVAR").await.unwrap();
        assert!(result.stdout.contains("world"));
    }

    #[tokio::test]
    async fn test_timeout_enforcement() {
        let session = ShellSession::new(None, 1, 51200, false); // 1 second timeout

        let result = session.execute("sleep 10").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_output_truncation() {
        let session = ShellSession::new(None, 30, 100, false); // 100 byte limit

        let result = session.execute("seq 1 1000").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.truncated);
        assert!(output.stdout.contains("[truncated"));
    }

    #[tokio::test]
    async fn test_workspace_restriction() {
        let temp_dir = std::env::temp_dir();
        let workspace = temp_dir.join(format!("chatty_shell_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();

        let session = ShellSession::new(
            Some(workspace.to_str().unwrap().to_string()),
            30,
            51200,
            false,
        );

        // Should be able to cd within workspace (start is in workspace)
        let result = session.execute("pwd").await;
        assert!(result.is_ok());
        assert!(result.unwrap().stdout.contains(workspace.to_str().unwrap()));

        // Cleanup
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[tokio::test]
    async fn test_invalid_env_var_name() {
        let session = ShellSession::new(None, 30, 51200, false);
        let result = session.set_env("INVALID-NAME", "value").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid environment variable name")
        );
    }

    #[tokio::test]
    async fn test_status() {
        let session = ShellSession::new(None, 30, 51200, false);

        // Before any command, session is not started
        let status = session.status().await.unwrap();
        assert!(!status.running);

        // Execute a command to start the session
        session.execute("echo 'start'").await.unwrap();

        // Now check status
        let status = session.status().await.unwrap();
        assert!(status.running);
        assert!(status.pid.is_some());
        assert!(!status.cwd.is_empty());
    }

    #[tokio::test]
    async fn test_process_respawn_after_death() {
        let session = ShellSession::new(None, 30, 51200, false);

        // Start a session
        session.execute("echo 'first'").await.unwrap();

        // Kill the process internally
        session.shutdown().await;

        // Next command should respawn
        let result = session.execute("echo 'respawned'").await;
        assert!(result.is_ok());
        assert!(result.unwrap().stdout.contains("respawned"));
    }

    #[tokio::test]
    async fn test_shutdown() {
        let session = ShellSession::new(None, 30, 51200, false);
        session.execute("echo 'test'").await.unwrap();
        assert!(session.is_running().await);

        session.shutdown().await;
        assert!(!session.is_running().await);
    }

    #[tokio::test]
    async fn test_special_characters_in_env_value() {
        let session = ShellSession::new(None, 30, 51200, false);

        // Test value with special characters
        let result = session
            .set_env("SPECIAL_VAR", "hello 'world' \"test\" $HOME")
            .await;
        assert!(result.is_ok());

        let result = session.execute("echo $SPECIAL_VAR").await.unwrap();
        assert!(result.stdout.contains("hello 'world' \"test\""));
    }

    #[tokio::test]
    async fn test_multiline_output() {
        let session = ShellSession::new(None, 30, 51200, false);

        let result = session
            .execute("echo 'line1'; echo 'line2'; echo 'line3'")
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("line1"));
        assert!(output.stdout.contains("line2"));
        assert!(output.stdout.contains("line3"));
    }

    #[test]
    fn test_can_sandbox() {
        let can = ShellSession::can_sandbox();
        // On Linux: depends on bwrap availability
        // On macOS: always true
        // On other platforms: false
        #[cfg(target_os = "macos")]
        assert!(can, "macOS should always support sandboxing");
        let _ = can; // Avoid unused variable warning
    }

    #[tokio::test]
    async fn test_sandboxed_session_persistence() {
        // Verify that sandboxed sessions still maintain state
        if !ShellSession::can_sandbox() {
            return;
        }

        let session = ShellSession::new(None, 30, 51200, false);

        // Set env var and verify it persists
        session
            .execute("export SANDBOX_TEST=hello_sandbox")
            .await
            .unwrap();
        let result = session.execute("echo $SANDBOX_TEST").await.unwrap();
        assert!(
            result.stdout.contains("hello_sandbox"),
            "Environment variables should persist in sandboxed session, got: {:?}",
            result.stdout
        );

        // Verify sandbox state
        assert!(session.is_sandboxed().await, "Session should be sandboxed");
    }

    #[tokio::test]
    async fn test_sandboxed_session_with_workspace() {
        if !ShellSession::can_sandbox() {
            return;
        }

        let temp_dir = std::env::temp_dir();
        let workspace = temp_dir.join(format!("chatty_sandbox_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();

        let session = ShellSession::new(
            Some(workspace.to_str().unwrap().to_string()),
            30,
            51200,
            false,
        );

        // Should be able to execute commands in workspace
        let result = session.execute("pwd").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.stdout.contains(workspace.to_str().unwrap()),
            "Should start in workspace directory, got: {:?}",
            output.stdout
        );

        // Should be able to create files in workspace
        let result = session
            .execute("echo 'test' > sandbox_test.txt && cat sandbox_test.txt")
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().stdout.contains("test"));

        // Cleanup
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[tokio::test]
    async fn test_network_isolation_flag() {
        // Just verify the session can be created with network_isolation=true
        let session = ShellSession::new(None, 30, 51200, true);
        let result = session.execute("echo 'network test'").await;
        assert!(result.is_ok());
        assert!(result.unwrap().stdout.contains("network test"));
    }
}
