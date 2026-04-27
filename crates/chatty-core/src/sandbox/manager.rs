use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};
use super::docker::DockerSandbox;
use super::monty::MontySandbox;

/// Per-conversation sandbox manager.
///
/// Lazily initializes one Docker container per language on first use.
/// Each container is reused across executions within the same conversation
/// to preserve state (installed packages, defined variables, etc.).
///
/// ## Backend selection
///
/// For Python code, the manager applies the following strategy:
///
/// 1. **Monty fast path** — if [`MontySandbox::can_handle`] returns `true`
///    and `python3` is available on the host, execute without Docker.
///    Typical latency: 5–50 ms (no container startup).
///
/// 2. **Docker fallback** — if Monty is unavailable, the code uses
///    unsupported imports, or Monty execution fails with a limitation
///    signal, fall back to a Docker container automatically.
///
/// For all other languages (JavaScript, TypeScript, Rust, Bash), Docker is
/// always used.
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
    ///
    /// For Python, [`MontySandbox`] is tried first as a zero-Docker fast path;
    /// execution falls through to Docker on any limitation or failure.
    pub async fn execute(
        &self,
        code: &str,
        language: &Language,
        expose_port: Option<u16>,
    ) -> Result<ExecutionResult> {
        // ── Monty fast path (Python only, no port publishing) ────────────────
        if *language == Language::Python && expose_port.is_none() && MontySandbox::can_handle(code)
        {
            match self.try_monty(code).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    info!(
                        error = %e,
                        "MontySandbox unavailable or failed; falling back to Docker"
                    );
                }
            }
        }

        // ── Docker path ───────────────────────────────────────────────────────
        self.execute_docker(code, language, expose_port).await
    }

    /// Attempt to run Python code via [`MontySandbox`].
    ///
    /// Returns `Err` if `python3` is not installed, the code exceeds resource
    /// limits, or any other execution failure occurs that should be retried
    /// with Docker.
    async fn try_monty(&self, code: &str) -> Result<ExecutionResult> {
        let sandbox = MontySandbox::new(self.config.clone());
        let result = sandbox.execute(code, &Language::Python).await?;

        // If the result looks like a Monty limitation (unsupported syntax,
        // missing module, etc.) rather than a user-code error, bubble up
        // the error so the caller falls back to Docker.
        //
        // We check both stderr and stdout because some scripts catch exceptions
        // and print them to stdout instead of letting them propagate to stderr.
        let combined = format!("{}\n{}", result.stderr, result.stdout);
        if result.exit_code != 0 && Self::is_monty_limitation(&combined) {
            anyhow::bail!(
                "Monty limitation detected (stderr: {}); retrying with Docker",
                result.stderr
            );
        }

        Ok(result)
    }

    /// Returns `true` if the stderr output suggests a Monty limitation rather
    /// than a legitimate user-code error.
    ///
    /// The heuristic checks for messages that indicate the code requires
    /// features Monty (or the subset supported by our subprocess backend) does
    /// not provide.  User-code errors (e.g. `ZeroDivisionError`) are *not*
    /// matched here — they should be surfaced as-is so the LLM can see them.
    fn is_monty_limitation(stderr: &str) -> bool {
        let signals = [
            "ModuleNotFoundError",
            "No module named",
            "ImportError",
            // Python 3.10+ match/case on an older interpreter
            "SyntaxError: invalid syntax",
        ];
        signals.iter().any(|s| stderr.contains(s))
    }

    /// Execute code using Docker, creating the container on first use.
    async fn execute_docker(
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

    /// Check if the Monty fast path is available on this system.
    ///
    /// Returns `true` when `python3` is found in `PATH`.
    ///
    /// Exposed for diagnostics (e.g. settings UI showing which backends are
    /// available).  The manager performs this check implicitly on each
    /// execution attempt via [`try_monty`] and falls back to Docker on failure.
    #[allow(dead_code)]
    pub async fn is_monty_available() -> bool {
        MontySandbox::is_available(None).await.unwrap_or(false)
    }
}
