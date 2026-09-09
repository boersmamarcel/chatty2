use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

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
        if *language == Language::Python {
            if expose_port.is_some() && !self.config.allow_docker_fallback {
                anyhow::bail!(
                    "Monty-only mode cannot expose ports. Enable Docker fallback or remove expose_port."
                );
            }

            if expose_port.is_none() && MontySandbox::can_handle(code) {
                match self.try_monty(code).await {
                    Ok(mut result) => {
                        result.clamp_output(self.config.max_output_bytes);
                        return Ok(result);
                    }
                    Err(e) => {
                        if !self.config.allow_docker_fallback {
                            anyhow::bail!(
                                "Monty-only mode could not execute this Python snippet: {}",
                                e
                            );
                        }
                        info!(
                            error = %e,
                            "MontySandbox unavailable or failed; falling back to Docker"
                        );
                    }
                }
            } else if !self.config.allow_docker_fallback {
                anyhow::bail!(
                    "Monty-only mode supports stdlib Python snippets only. Avoid third-party imports, subprocess/socket usage, and features that require Docker fallback."
                );
            }
        } else if !self.config.allow_docker_fallback {
            anyhow::bail!(
                "Monty-only mode only supports Python. Enable Docker fallback to run {:?} code.",
                language
            );
        }

        // ── Docker path ───────────────────────────────────────────────────────
        let mut result = self.execute_docker(code, language, expose_port).await?;
        result.clamp_output(self.config.max_output_bytes);
        Ok(result)
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
            if self.config.allow_docker_fallback {
                anyhow::bail!(
                    "Monty limitation detected (stderr: {}); retrying with Docker",
                    result.stderr
                );
            } else {
                anyhow::bail!(
                    "Monty-only mode detected an unsupported Python feature: {}",
                    result.stderr
                );
            }
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

        if needs_recreate
            && let Some(old) = guard.remove(language)
            && let Err(e) = old.destroy().await
        {
            warn!(?language, error = %e, "failed to destroy sandbox container being recreated");
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

    /// Destroy all sandbox containers now, awaiting completion.
    ///
    /// Dropping the manager does this automatically in the background (see
    /// `impl Drop` below) — call this instead when a caller wants a graceful,
    /// awaited shutdown rather than a detached best-effort task, e.g. to
    /// surface a removal failure to the user.
    pub async fn destroy(&self) -> Result<()> {
        destroy_all(&self.sandboxes).await
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

/// Drain `sandboxes` and destroy every container in it.
async fn destroy_all(sandboxes: &Mutex<HashMap<Language, Box<dyn SandboxBackend>>>) -> Result<()> {
    let mut guard = sandboxes.lock().await;
    for (_, sandbox) in guard.drain() {
        sandbox.destroy().await?;
    }
    Ok(())
}

impl Drop for SandboxManager {
    /// Best-effort backstop: `destroy()` is the graceful, awaited path,
    /// called explicitly by callers that want to know removal succeeded.
    /// This covers everywhere else a manager's last reference simply goes
    /// out of scope (a conversation is deleted, its agent is rebuilt on a
    /// model switch or tool-set change, ...) so a sandbox container never
    /// outlives every Rust-side reference to it. Runs as a detached task
    /// since Docker removal is an async HTTP call and `Drop::drop` isn't.
    fn drop(&mut self) {
        let sandboxes = self.sandboxes.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("no Tokio runtime available to destroy sandbox containers on drop");
            return;
        };
        handle.spawn(async move {
            if let Err(e) = destroy_all(&sandboxes).await {
                warn!(error = %e, "failed to destroy sandbox container on drop");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::backend::ExecutionResult;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A fake backend that just records whether `destroy()` ran, so these
    /// tests don't need a real Docker daemon.
    struct MockBackend {
        destroyed: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl SandboxBackend for MockBackend {
        async fn execute(&self, _code: &str, _language: &Language) -> Result<ExecutionResult> {
            unimplemented!("not exercised by these tests")
        }

        async fn destroy(self: Box<Self>) -> Result<()> {
            self.destroyed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn has_port_exposed(&self, _port: u16) -> bool {
            false
        }

        async fn is_available(_docker_host: Option<&str>) -> Result<bool> {
            Ok(true)
        }
    }

    fn manager_with_mock(destroyed: Arc<AtomicBool>) -> SandboxManager {
        let manager = SandboxManager::new(SandboxConfig::default());
        manager
            .sandboxes
            .try_lock()
            .expect("uncontended in test setup")
            .insert(
                Language::Python,
                Box::new(MockBackend { destroyed }) as Box<dyn SandboxBackend>,
            );
        manager
    }

    #[tokio::test]
    async fn explicit_destroy_awaits_completion_and_empties_the_map() {
        let destroyed = Arc::new(AtomicBool::new(false));
        let manager = manager_with_mock(destroyed.clone());

        manager.destroy().await.expect("destroy should succeed");

        assert!(destroyed.load(Ordering::SeqCst));
        assert!(manager.sandboxes.lock().await.is_empty());
    }

    #[tokio::test]
    async fn dropping_the_manager_destroys_tracked_sandboxes() {
        let destroyed = Arc::new(AtomicBool::new(false));
        let manager = manager_with_mock(destroyed.clone());

        drop(manager);

        // Drop spawns a detached task onto the runtime; give it a chance to
        // run before asserting.
        for _ in 0..100 {
            if destroyed.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        assert!(
            destroyed.load(Ordering::SeqCst),
            "dropping the manager should destroy its tracked sandboxes in the background"
        );
    }
}
