use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

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

use crate::tools::ToolError;

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
}

impl SubAgentTool {
    pub fn new(model_id: String, auto_approve: bool, available_model_ids: Vec<String>) -> Self {
        Self {
            model_id,
            auto_approve,
            available_model_ids,
        }
    }
}

impl Tool for SubAgentTool {
    const NAME: &'static str = "sub_agent";
    type Error = ToolError;
    type Args = SubAgentArgs;
    type Output = SubAgentOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "sub_agent".to_string(),
            description: "Delegate a task to an independent sub-agent that has access to the \
                         same tools as you. The sub-agent runs autonomously in its own process, \
                         executes the task (including any tool calls it needs), and returns the \
                         result. Use this to parallelize work or to isolate complex sub-tasks. \
                         Each sub-agent starts with a fresh conversation context — provide all \
                         necessary context in the task description. \
                         You can optionally specify a different model for the sub-agent."
                .to_string(),
            parameters: serde_json::json!({
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
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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

        // Run the subprocess in a blocking task to avoid blocking the async runtime.
        let result =
            tokio::task::spawn_blocking(move || run_sub_agent(exe, model_id, task, auto_approve))
                .await
                .map_err(|e| {
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

/// Spawn chatty-tui in headless mode and collect its output.
fn run_sub_agent(
    executable: PathBuf,
    model_id: String,
    task: String,
    auto_approve: bool,
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

    if auto_approve {
        cmd.arg("--auto-approve");
    }

    info!(exe = ?executable, "Launching headless sub-agent");

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to launch sub-agent: {e}")),
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return Err(format!("Sub-agent process failed: {e}")),
    };

    // Log stderr for debugging (tool progress info)
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    if !stderr_str.is_empty() {
        for line in stderr_str.lines() {
            info!(sub_agent_progress = %line);
        }
    }

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
