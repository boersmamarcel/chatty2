use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::PathBuf;
use tracing::{info, warn};

use crate::models::message_types::ToolSource;
use crate::tools::ToolError;
use crate::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};

/// Prefix for structured headless progress lines on stderr.
///
/// Format:
/// - `CHATTY_PROGRESS\ttool_started\t{name}`
/// - `CHATTY_PROGRESS\ttool_finished\t{name}\t{ok|err}`
pub const CHATTY_PROGRESS_PREFIX: &str = "CHATTY_PROGRESS\t";

/// Maximum number of characters from stderr to include in error messages.
const STDERR_PREVIEW_CHARS: usize = 500;

/// Arguments for the sub_agent tool
#[derive(Deserialize, Serialize)]
pub struct SubAgentArgs {
    /// The task or prompt to delegate to the sub-agent.
    pub task: String,
    /// Optional model ID to use for the sub-agent. If omitted, the parent's
    /// model is used. Use `list_tools` to see the current model or check
    /// the available model IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Output from the sub_agent tool
#[derive(Debug, Serialize)]
pub struct SubAgentOutput {
    /// The sub-agent's response text.
    pub response: String,
    /// Whether the sub-agent completed successfully.
    pub success: bool,
}

/// Tool that spawns a sub-agent (chatty-tui in headless mode) to handle a
/// delegated task autonomously.
///
/// The master agent can use this tool to spin up independent sub-agents that
/// have access to the same tool set. Each sub-agent runs in its own process,
/// executes the task, and returns the result. This enables the master agent
/// to parallelize work by launching multiple sub-agents for different tasks.
///
/// The sub-agent may optionally use a different model than its parent.
#[derive(Clone)]
pub struct SubAgentTool {
    /// Model ID the sub-agent uses by default (inherits from the parent conversation).
    model_id: String,
    /// Whether to auto-approve tool calls in the sub-agent.
    auto_approve: bool,
    /// Available model IDs for validation (empty = skip validation).
    available_model_ids: Vec<String>,
    /// Shared slot with `InvokeAgentTool` for compact live progress in the parent UI.
    progress_slot: InvokeAgentProgressSlot,
}

impl SubAgentTool {
    pub fn new(
        model_id: String,
        auto_approve: bool,
        available_model_ids: Vec<String>,
        progress_slot: InvokeAgentProgressSlot,
    ) -> Self {
        Self {
            model_id,
            auto_approve,
            available_model_ids,
            progress_slot,
        }
    }

    fn send_progress(&self, event: InvokeAgentProgress) {
        send_progress(&self.progress_slot, event);
    }
}

/// Returns true when `line` is a structured headless progress event.
pub fn is_chatty_progress_line(line: &str) -> bool {
    line.starts_with(CHATTY_PROGRESS_PREFIX)
}

/// Parse a structured progress line into compact UI text.
///
/// Unprefixed token soup and human tool-format lines return `None`.
fn parse_progress_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix(CHATTY_PROGRESS_PREFIX)?;
    let mut parts = rest.split('\t');
    let kind = parts.next()?;
    let name = parts.next()?.trim();
    if name.is_empty() {
        return None;
    }
    match kind {
        "tool_started" => Some(name.to_string()),
        "tool_finished" => match parts.next().unwrap_or("ok") {
            "ok" => Some(format!("\u{2713} {name}")),
            _ => Some(format!("\u{2717} {name}")),
        },
        _ => None,
    }
}

fn send_progress(slot: &InvokeAgentProgressSlot, event: InvokeAgentProgress) {
    let guard = slot.lock();
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(event);
    }
}

impl Tool for SubAgentTool {
    const NAME: &'static str = "sub_agent";
    type Error = ToolError;
    type Args = SubAgentArgs;
    type Output = SubAgentOutput;

    fn description(&self) -> String {
        "Delegate a task to an independent sub-agent that has access to the \
                         same tools as you. The sub-agent runs autonomously in its own process, \
                         executes the task (including any tool calls it needs), and returns the \
                         result. Use this to parallelize work or to isolate complex sub-tasks. \
                         Each sub-agent starts with a fresh conversation context — provide all \
                         necessary context in the task description. \
                         You can optionally specify a different model for the sub-agent."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A detailed description of the task for the sub-agent. \
                                   Include all context the sub-agent needs since it does not \
                                   share conversation history with the parent."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model ID to use for the sub-agent. If omitted, \
                                   the sub-agent uses the same model as the parent. Use this \
                                   to pick a faster/cheaper model for simple tasks or a more \
                                   capable model for complex ones."
                }
            },
            "required": ["task"]
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
        let task = args.task.trim().to_string();
        if task.is_empty() {
            return Err(ToolError::OperationFailed(
                "Task description cannot be empty".to_string(),
            ));
        }

        // Resolve model: use the requested model if provided, else fall back
        // to the parent's model.
        let model_id = if let Some(requested) = &args.model {
            let requested = requested.trim().to_string();
            if requested.is_empty() {
                self.model_id.clone()
            } else if !self.available_model_ids.is_empty()
                && !self.available_model_ids.contains(&requested)
            {
                return Err(ToolError::OperationFailed(format!(
                    "Unknown model '{}'. Available models: {}",
                    requested,
                    self.available_model_ids.join(", ")
                )));
            } else {
                requested
            }
        } else {
            self.model_id.clone()
        };

        info!(
            task_len = task.len(),
            model = %model_id,
            "Launching sub-agent for delegated task"
        );

        // Find the chatty-tui binary: check same directory as current binary first,
        // then fall back to PATH resolution (may fail at spawn time if not found).
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("chatty-tui")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| {
                warn!("chatty-tui not found next to current binary, falling back to PATH");
                PathBuf::from("chatty-tui")
            });

        let auto_approve = self.auto_approve;
        let progress_slot = self.progress_slot.clone();

        // Run the subprocess in a blocking task to avoid blocking the async runtime.
        let result = tokio::task::spawn_blocking(move || {
            run_sub_agent_with_progress(exe, model_id, task, auto_approve, progress_slot)
        })
        .await
        .map_err(|e| {
            self.send_progress(InvokeAgentProgress::Finished {
                success: false,
                result: Some(e.to_string()),
            });
            ToolError::OperationFailed(format!("Sub-agent task failed to complete: {e}"))
        })?;

        match result {
            Ok(stdout) => {
                let response = stdout.trim().to_string();
                if response.is_empty() {
                    Ok(SubAgentOutput {
                        response: "Sub-agent completed with no output.".to_string(),
                        success: true,
                    })
                } else {
                    Ok(SubAgentOutput {
                        response,
                        success: true,
                    })
                }
            }
            Err(e) => Ok(SubAgentOutput {
                response: format!("Sub-agent failed: {e}"),
                success: false,
            }),
        }
    }
}

/// Emit Started/Finished around the child process so the parent UI row
/// never sticks on Running.
fn run_sub_agent_with_progress(
    executable: PathBuf,
    model_id: String,
    task: String,
    auto_approve: bool,
    progress_slot: InvokeAgentProgressSlot,
) -> Result<String, String> {
    send_progress(
        &progress_slot,
        InvokeAgentProgress::Started {
            agent_name: "sub_agent".to_string(),
            prompt: task.clone(),
            source: ToolSource::Local,
        },
    );
    let result = run_sub_agent(
        executable,
        model_id,
        task,
        auto_approve,
        progress_slot.clone(),
    );
    match &result {
        Ok(stdout) => send_progress(
            &progress_slot,
            InvokeAgentProgress::Finished {
                success: true,
                result: Some(stdout.clone()),
            },
        ),
        Err(e) => send_progress(
            &progress_slot,
            InvokeAgentProgress::Finished {
                success: false,
                result: Some(e.clone()),
            },
        ),
    }
    result
}

/// Spawn chatty-tui in headless mode and collect its output.
///
/// Stderr is drained live: only `CHATTY_PROGRESS` lines are forwarded to the
/// parent UI. stdout is returned as the tool result for the parent model.
fn run_sub_agent(
    executable: PathBuf,
    model_id: String,
    task: String,
    auto_approve: bool,
    progress_slot: InvokeAgentProgressSlot,
) -> Result<String, String> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(&executable);
    cmd.arg("--headless")
        .arg("--model")
        .arg(&model_id)
        .arg("--message")
        .arg(&task)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "macos")]
    if let Some(exe_dir) = executable.parent() {
        let frameworks_dir = exe_dir.join("../Frameworks");
        if frameworks_dir.join("libpdfium.dylib").exists() {
            cmd.env("CHATTY_PDFIUM_LIB_DIR", frameworks_dir);
        }
    }

    if auto_approve {
        cmd.arg("--auto-approve");
    }

    info!(exe = ?executable, "Launching headless sub-agent");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to launch sub-agent: {e}")),
    };

    let stderr = child.stderr.take();
    let slot_for_drain = progress_slot.clone();
    let stderr_thread = std::thread::spawn(move || {
        let mut collected = String::new();
        if let Some(stderr) = stderr {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                info!(sub_agent_progress = %line);
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
                if let Some(text) = parse_progress_line(&line) {
                    send_progress(&slot_for_drain, InvokeAgentProgress::Text(text));
                }
            }
        }
        collected
    });

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            let _ = stderr_thread.join();
            return Err(format!("Sub-agent process failed: {e}"));
        }
    };

    let stderr_str = stderr_thread.join().unwrap_or_default();

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let exit_code = output.status.code();
        let stderr_preview = stderr_str
            .chars()
            .take(STDERR_PREVIEW_CHARS)
            .collect::<String>();
        Err(format!(
            "Sub-agent failed (exit {:?}): {}",
            exit_code, stderr_preview
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::install_progress_channel;
    use parking_lot::Mutex;
    use rig_agent::tool::{Tool, ToolContext};
    use std::sync::Arc;

    fn dummy_slot() -> InvokeAgentProgressSlot {
        Arc::new(Mutex::new(None))
    }

    #[test]
    fn parse_progress_line_started_and_finished() {
        assert_eq!(
            parse_progress_line("CHATTY_PROGRESS\ttool_started\tread_file").as_deref(),
            Some("read_file")
        );
        assert_eq!(
            parse_progress_line("CHATTY_PROGRESS\ttool_finished\tread_file\tok").as_deref(),
            Some("✓ read_file")
        );
        assert_eq!(
            parse_progress_line("CHATTY_PROGRESS\ttool_finished\tshell_execute\terr").as_deref(),
            Some("✗ shell_execute")
        );
    }

    #[test]
    fn parse_progress_line_ignores_unprefixed_noise() {
        assert_eq!(parse_progress_line("token soup without prefix"), None);
        assert_eq!(
            parse_progress_line("  [tool: read_file] \u{27f3} running"),
            None
        );
        assert_eq!(parse_progress_line("CHATTY_PROGRESS\tunknown\tfoo"), None);
        assert!(!is_chatty_progress_line("hello"));
        assert!(is_chatty_progress_line(
            "CHATTY_PROGRESS\ttool_started\tread_file"
        ));
    }

    #[tokio::test]
    async fn test_empty_task_rejected() {
        let slot = dummy_slot();
        let mut rx = install_progress_channel(&slot);
        let tool = SubAgentTool::new("model-1".into(), false, Vec::new(), slot);
        let result = tool
            .call(
                &mut ToolContext::new(),
                SubAgentArgs {
                    task: "   ".to_string(),
                    model: None,
                },
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("cannot be empty"),
            "unexpected error: {err}"
        );
        assert!(
            rx.try_recv().is_err(),
            "empty task must not emit progress events"
        );
    }

    #[tokio::test]
    async fn test_model_validation_rejects_unknown() {
        let tool = SubAgentTool::new(
            "default-model".into(),
            false,
            vec!["model-a".into(), "model-b".into()],
            dummy_slot(),
        );
        let result = tool
            .call(
                &mut ToolContext::new(),
                SubAgentArgs {
                    task: "do something".to_string(),
                    model: Some("nonexistent".to_string()),
                },
            )
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Unknown model"),
            "unexpected error: {err}"
        );
        assert!(err.to_string().contains("model-a"));
        assert!(err.to_string().contains("model-b"));
    }

    #[tokio::test]
    async fn test_model_validation_accepts_known() {
        let tool = SubAgentTool::new(
            "default-model".into(),
            false,
            vec!["model-a".into(), "model-b".into()],
            dummy_slot(),
        );
        // The model validation passes, but the call will fail later when
        // trying to spawn the chatty-tui binary (which doesn't exist in tests).
        // We verify it does NOT fail with "Unknown model".
        let result = tool
            .call(
                &mut ToolContext::new(),
                SubAgentArgs {
                    task: "do something".to_string(),
                    model: Some("model-a".to_string()),
                },
            )
            .await;
        match result {
            Err(e) => assert!(
                !e.to_string().contains("Unknown model"),
                "should not reject known model, got: {e}"
            ),
            Ok(output) => {
                // If it somehow succeeds or returns a SubAgentOutput with
                // success=false (binary not found), that's also fine — model
                // validation passed.
                assert!(!output.success || !output.response.is_empty());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_progress_forwards_prefixed_lines_and_returns_stdout() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let script = dir.path().join("fake-tui");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             printf 'token soup' >&2\n\
             echo >&2\n\
             echo 'CHATTY_PROGRESS\ttool_started\tread_file' >&2\n\
             echo '  [tool: read_file] running' >&2\n\
             echo 'CHATTY_PROGRESS\ttool_finished\tread_file\tok' >&2\n\
             echo 'noise' >&2\n\
             echo 'final answer'\n",
        )
        .expect("write fixture");
        let mut perms = std::fs::metadata(&script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");

        let slot = dummy_slot();
        let mut rx = install_progress_channel(&slot);
        let stdout = run_sub_agent_with_progress(
            script,
            "model-1".into(),
            "do the task".into(),
            false,
            slot,
        )
        .expect("fixture should succeed");
        assert_eq!(stdout, "final answer");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(
            matches!(
                events.first(),
                Some(InvokeAgentProgress::Started {
                    agent_name,
                    prompt,
                    source: ToolSource::Local,
                }) if agent_name == "sub_agent" && prompt == "do the task"
            ),
            "expected Started first, got {events:?}"
        );
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                InvokeAgentProgress::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["read_file", "✓ read_file"]);
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("token soup") || t.contains("noise")),
            "unprefixed stderr must not be forwarded: {texts:?}"
        );
        assert!(
            matches!(
                events.last(),
                Some(InvokeAgentProgress::Finished {
                    success: true,
                    result: Some(result),
                }) if result == "final answer"
            ),
            "expected Finished last, got {events:?}"
        );
    }
}
