use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of executing code in a sandbox container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub timed_out: bool,
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
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            language: Language::Python,
            memory_mb: 512,
            cpu_quota: 50000,
            timeout_secs: 30,
            network: false,
        }
    }
}

/// Supported programming languages for sandbox execution.
#[derive(Debug, Clone, PartialEq)]
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
    #[allow(dead_code)]
    async fn destroy(self: Box<Self>) -> Result<()>;

    /// Health check — is the backend available?
    #[allow(dead_code)]
    async fn is_available() -> Result<bool>
    where
        Self: Sized;
}
