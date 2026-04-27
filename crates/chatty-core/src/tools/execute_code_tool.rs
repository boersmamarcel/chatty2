use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::message_types::ExecutionEngine;
use crate::sandbox::backend::Language;
use crate::sandbox::manager::SandboxManager;
use crate::tools::ToolError;

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecuteCodeArgs {
    /// The programming language: python, javascript, typescript, rust, bash
    pub language: String,
    /// The code to execute
    pub code: String,
    /// Optional container port to publish to the host (e.g. 8080 for a web server).
    /// When set, the container's port is bound to a random localhost port.
    /// The actual host port is returned in port_mappings.
    pub expose_port: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct ExecuteCodeOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub timed_out: bool,
    pub execution_engine: ExecutionEngine,
    /// Published port mappings: container_port → host_port.
    /// When a web server is running, tell the user to connect to http://localhost:<host_port>
    pub port_mappings: std::collections::HashMap<u16, u16>,
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Execute code in an isolated sandbox.
///
/// Supported languages: python, javascript, typescript, rust, bash.
/// Python may use the fast Monty interpreter for simple snippets and fall back
/// to Docker automatically; other languages use Docker. The sandbox preserves
/// state (variables, installed packages) throughout the conversation. No
/// network access by default.
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
    type Error = ToolError;
    type Args = ExecuteCodeArgs;
    type Output = ExecuteCodeOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "execute_code",
            "description": "Execute code in an isolated sandbox. Supported languages: python, javascript, typescript, rust, bash. Python may use the built-in Monty interpreter for simple snippets and automatically fall back to Docker when a fuller environment is needed; other languages use Docker. The sandbox preserves state (variables, installed packages) throughout the conversation. The output includes execution_engine so you can tell the user whether Monty or Docker ran the code. To start a web server the user can access, set expose_port to the port your server listens on (e.g. 8080). The actual host port is returned in port_mappings — tell the user to open http://localhost:<host_port>.",
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
                    },
                    "expose_port": {
                        "type": "integer",
                        "description": "Optional container port to publish to the host. Use when starting a web server so the user can access it. The mapped host port is returned in port_mappings."
                    }
                },
                "required": ["language", "code"]
            }
        }))
        .expect("valid tool definition")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let language = Language::parse(&args.language);
        let result = self
            .manager
            .execute(&args.code, &language, args.expose_port)
            .await?;

        Ok(ExecuteCodeOutput {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
            timed_out: result.timed_out,
            execution_engine: result.execution_engine,
            port_mappings: result.port_mappings,
        })
    }
}
