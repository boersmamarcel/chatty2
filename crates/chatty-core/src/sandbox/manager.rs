use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};
use super::docker::DockerSandbox;

/// Per-conversation sandbox manager.
///
/// Lazily initializes one Docker container per language on first use.
/// Each container is reused across executions within the same conversation
/// to preserve state (installed packages, defined variables, etc.).
pub struct SandboxManager {
    sandboxes: Arc<Mutex<HashMap<Language, Box<dyn SandboxBackend>>>>,
    config: SandboxConfig,
}

impl SandboxManager {
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Execute code in the sandbox.
    ///
    /// A container per language is created on first use and reused to preserve state.
    /// If `expose_port` is specified and the existing container does not have that port
    /// published, the container is recreated (state is reset for that language).
    pub async fn execute(
        &self,
        code: &str,
        language: &Language,
        expose_port: Option<u16>,
    ) -> Result<ExecutionResult> {
        let mut guard = self.sandboxes.lock().await;

        // Recreate the container if the requested port isn't already exposed.
        let needs_recreate = expose_port.is_some_and(|port| {
            guard
                .get(language)
                .is_some_and(|sb| !sb.has_port_exposed(port))
        });

        if needs_recreate && let Some(old) = guard.remove(language) {
            let _ = old.destroy().await;
        }

        if !guard.contains_key(language) {
            let mut config = self.config.clone();
            config.language = language.clone();
            if let Some(port) = expose_port {
                config.expose_ports = vec![port];
            }
            let sandbox = DockerSandbox::create(config).await?;
            guard.insert(language.clone(), Box::new(sandbox));
        }

        guard[language].execute(code, language).await
    }

    /// Destroy all sandbox containers. Call when the conversation ends.
    #[allow(dead_code)]
    pub async fn destroy(&self) -> Result<()> {
        let mut guard = self.sandboxes.lock().await;
        for (_, sandbox) in guard.drain() {
            sandbox.destroy().await?;
        }
        Ok(())
    }

    /// Check if Docker is available on this system.
    #[allow(dead_code)]
    pub async fn is_docker_available(docker_host: Option<&str>) -> bool {
        DockerSandbox::is_available(docker_host)
            .await
            .unwrap_or(false)
    }
}
