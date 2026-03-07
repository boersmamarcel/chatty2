use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};
use super::docker::DockerSandbox;

/// Per-conversation sandbox manager.
///
/// Lazily initializes a Docker container on the first code execution.
/// The container is reused across executions within the same conversation
/// to preserve state (installed packages, defined variables, etc.).
pub struct SandboxManager {
    sandbox: Arc<Mutex<Option<Box<dyn SandboxBackend>>>>,
    config: SandboxConfig,
}

impl SandboxManager {
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            sandbox: Arc::new(Mutex::new(None)),
            config,
        }
    }

    /// Execute code in the sandbox, creating the container on first use.
    pub async fn execute(&self, code: &str, language: &Language) -> Result<ExecutionResult> {
        let mut guard = self.sandbox.lock().await;

        // Lazy init: create container on first use
        if guard.is_none() {
            let mut config = self.config.clone();
            config.language = language.clone();
            let sandbox = DockerSandbox::create(config).await?;
            *guard = Some(Box::new(sandbox));
        }

        guard.as_ref().unwrap().execute(code, language).await
    }

    /// Destroy the sandbox container. Call when the conversation ends.
    #[allow(dead_code)]
    pub async fn destroy(&self) -> Result<()> {
        let mut guard = self.sandbox.lock().await;
        if let Some(sandbox) = guard.take() {
            sandbox.destroy().await?;
        }
        Ok(())
    }

    /// Check if Docker is available on this system.
    #[allow(dead_code)]
    pub async fn is_docker_available() -> bool {
        DockerSandbox::is_available().await.unwrap_or(false)
    }
}
