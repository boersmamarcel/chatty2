use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::models::message_types::ExecutionEngine;

/// Result of executing code in a sandbox container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub timed_out: bool,
    /// Exposed port mappings: container_port → host_port (populated when ports are published)
    #[serde(default)]
    pub port_mappings: HashMap<u16, u16>,
    /// Which backend actually executed the code.
    pub execution_engine: ExecutionEngine,
}

impl ExecutionResult {
    pub fn clamp_output(&mut self, max_output_bytes: usize) {
        if max_output_bytes == 0 {
            self.stdout.clear();
            self.stderr.clear();
            return;
        }

        let total_len = self.stdout.len() + self.stderr.len();
        if total_len <= max_output_bytes {
            return;
        }

        if self.stderr.is_empty() {
            truncate_output_stream(&mut self.stdout, max_output_bytes, "stdout");
            return;
        }

        if self.stdout.is_empty() {
            truncate_output_stream(&mut self.stderr, max_output_bytes, "stderr");
            return;
        }

        let preferred_stdout = (max_output_bytes * 3 / 4).max(256).min(max_output_bytes);
        let mut stdout_budget = preferred_stdout;
        let mut stderr_budget = max_output_bytes.saturating_sub(stdout_budget);

        if self.stdout.len() < stdout_budget {
            stderr_budget += stdout_budget - self.stdout.len();
            stdout_budget = self.stdout.len();
        }
        if self.stderr.len() < stderr_budget {
            stdout_budget += stderr_budget - self.stderr.len();
            stderr_budget = self.stderr.len();
        }

        truncate_output_stream(&mut self.stdout, stdout_budget, "stdout");
        truncate_output_stream(&mut self.stderr, stderr_budget, "stderr");
    }
}

fn truncate_output_stream(output: &mut String, max_output_bytes: usize, label: &str) {
    if output.len() <= max_output_bytes {
        return;
    }

    let original_len = output.len();
    let mut end = max_output_bytes;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    output.push_str(&format!(
        "\n... [{label} truncated by {} bytes; keep prints compact]",
        original_len - end
    ));
}

/// Configuration for a sandbox container.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub language: Language,
    /// Memory limit in megabytes (default: 512)
    pub memory_mb: u64,
    /// CPU quota in microseconds per 100ms period (default: 50000 = 50% of one core)
    pub cpu_quota: i64,
    /// Maximum execution time in seconds (default: 30)
    pub timeout_secs: u64,
    /// Whether network access is allowed (default: false)
    pub network: bool,
    /// Host path to mount at /workspace inside the container (default: None)
    pub workspace_path: Option<String>,
    /// Container ports to publish to the host (default: empty = no ports published)
    pub expose_ports: Vec<u16>,
    /// Custom Docker host URI or socket path. When None, fallback discovery is used.
    pub docker_host: Option<String>,
    /// Allow Docker fallback when Monty cannot handle the request.
    pub allow_docker_fallback: bool,
    /// Maximum tool output bytes returned to the model across stdout/stderr.
    pub max_output_bytes: usize,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            language: Language::Python,
            memory_mb: 512,
            cpu_quota: 50000,
            timeout_secs: 30,
            network: false,
            workspace_path: None,
            expose_ports: vec![],
            docker_host: None,
            allow_docker_fallback: true,
            max_output_bytes: 4096,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn clamp_output_truncates_large_stdout() {
        let original_len = 128;
        let mut result = ExecutionResult {
            stdout: "x".repeat(original_len),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
            port_mappings: HashMap::new(),
            execution_engine: ExecutionEngine::Monty,
        };

        result.clamp_output(48);

        assert!(result.stdout.len() < original_len);
        assert!(result.stdout.contains("[stdout truncated"));
    }

    #[test]
    fn clamp_output_preserves_both_streams() {
        let mut result = ExecutionResult {
            stdout: "a".repeat(160),
            stderr: "b".repeat(80),
            exit_code: 0,
            timed_out: false,
            port_mappings: HashMap::new(),
            execution_engine: ExecutionEngine::Docker,
        };

        result.clamp_output(96);

        assert!(result.stdout.contains("[stdout truncated"));
        assert!(result.stderr.contains("[stderr truncated"));
    }
}

/// Supported programming languages for sandbox execution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Language {
    Python,
    JavaScript,
    TypeScript,
    Rust,
    Bash,
}

impl Language {
    /// Docker image to use for this language.
    pub fn docker_image(&self) -> &'static str {
        match self {
            Language::Python => "python:3.12-slim",
            Language::JavaScript => "node:20-slim",
            Language::TypeScript => "node:20-slim",
            Language::Rust => "rust:1.76-slim",
            Language::Bash => "ubuntu:22.04",
        }
    }

    /// File extension for source files in this language.
    pub fn file_extension(&self) -> &'static str {
        match self {
            Language::Python => "py",
            Language::JavaScript => "js",
            Language::TypeScript => "ts",
            Language::Rust => "rs",
            Language::Bash => "sh",
        }
    }

    /// Shell command to run a source file in this language.
    pub fn run_command(&self, filename: &str) -> Vec<String> {
        match self {
            Language::Python => vec!["python3".into(), filename.into()],
            Language::JavaScript => vec!["node".into(), filename.into()],
            Language::TypeScript => vec!["npx".into(), "ts-node".into(), filename.into()],
            Language::Rust => vec![
                "sh".into(),
                "-c".into(),
                format!("rustc {} -o /tmp/out && /tmp/out", filename),
            ],
            Language::Bash => vec!["bash".into(), filename.into()],
        }
    }

    /// Parse a language name string into a Language variant.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "python" | "py" => Language::Python,
            "javascript" | "js" => Language::JavaScript,
            "typescript" | "ts" => Language::TypeScript,
            "rust" | "rs" => Language::Rust,
            "bash" | "sh" | "shell" => Language::Bash,
            _ => Language::Python,
        }
    }
}

/// Trait abstracting sandbox backends (Docker, gVisor, E2B, etc.)
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Execute code in the sandbox. State (installed packages, defined variables)
    /// is preserved within a single conversation.
    async fn execute(&self, code: &str, language: &Language) -> Result<ExecutionResult>;

    /// Destroy the sandbox and remove its container.
    async fn destroy(self: Box<Self>) -> Result<()>;

    /// Returns true if the given container port is published to the host.
    fn has_port_exposed(&self, port: u16) -> bool;

    /// Health check — is the backend available?
    #[allow(dead_code)]
    async fn is_available(docker_host: Option<&str>) -> Result<bool>
    where
        Self: Sized;
}
