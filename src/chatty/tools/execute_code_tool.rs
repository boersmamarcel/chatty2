use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::chatty::sandbox::backend::Language;
use crate::chatty::sandbox::manager::SandboxManager;

// ── Error types ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ExecuteCodeError {
    #[error("Sandbox error: {0}")]
    SandboxError(#[from] anyhow::Error),
}

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecuteCodeArgs {
    /// The programming language: python, javascript, typescript, rust, bash
    pub language: String,
    /// The code to execute
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct ExecuteCodeOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub timed_out: bool,
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Execute code in an isolated Docker sandbox.
///
/// Supported languages: python, javascript, typescript, rust, bash.
/// The sandbox preserves state (variables, installed packages) throughout
/// the conversation. No network access by default.
#[derive(Clone)]
pub struct ExecuteCodeTool {
    manager: Arc<SandboxManager>,
}

impl ExecuteCodeTool {
    pub fn new(manager: Arc<SandboxManager>) -> Self {
        Self { manager }
    }
}

impl Tool for ExecuteCodeTool {
    const NAME: &'static str = "execute_code";
    type Error = ExecuteCodeError;
    type Args = ExecuteCodeArgs;
    type Output = ExecuteCodeOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "execute_code",
            "description": "Execute code in an isolated Docker sandbox. Supported languages: python, javascript, typescript, rust, bash. The sandbox preserves state (variables, installed packages) throughout the conversation. No network access.",
            "parameters": {
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "The programming language: python, javascript, typescript, rust, bash",
                        "enum": ["python", "javascript", "typescript", "rust", "bash"]
                    },
                    "code": {
                        "type": "string",
                        "description": "The code to execute"
                    }
                },
                "required": ["language", "code"]
            }
        }))
        .expect("valid tool definition")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let language = Language::parse(&args.language);
        let result = self.manager.execute(&args.code, &language).await?;

        Ok(ExecuteCodeOutput {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
            timed_out: result.timed_out,
        })
    }
}
