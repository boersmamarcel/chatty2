use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::bash_executor::{BashExecutor, BashToolInput, BashToolOutput};

/// Error type for bash tool execution
#[derive(Debug, thiserror::Error)]
pub enum BashToolError {
    #[error("Execution error: {0}")]
    ExecutionError(#[from] anyhow::Error),
}

/// Bash command execution tool arguments
#[derive(Deserialize, Serialize)]
pub struct BashToolArgs {
    pub command: String,
}

/// Bash command execution tool for rig agents
#[derive(Clone)]
pub struct BashTool {
    executor: Arc<BashExecutor>,
}

impl BashTool {
    pub fn new(executor: Arc<BashExecutor>) -> Self {
        Self { executor }
    }
}

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = BashToolError;
    type Args = BashToolArgs;
    type Output = BashToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute bash commands in a sandboxed environment. Use this to run shell commands, \
                         scripts, or system utilities. All commands require user approval before execution. \
                         Output is limited to 50KB and execution times out after 30 seconds. \
                         \
                         Best practices:\n\
                         - Explain what you're doing before requesting execution\n\
                         - Use simple, focused commands\n\
                         - Check exit codes to verify success\n\
                         - Be cautious with file operations\n\
                         \
                         Examples:\n\
                         - List files: ls -la\n\
                         - Check disk usage: df -h\n\
                         - Search files: grep -r 'pattern' .\n\
                         - Run tests: cargo test"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute. Single commands work best. For multi-step operations, use && to chain commands."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let input = BashToolInput {
            command: args.command,
        };
        self.executor
            .execute(input)
            .await
            .map_err(BashToolError::ExecutionError)
    }
}
