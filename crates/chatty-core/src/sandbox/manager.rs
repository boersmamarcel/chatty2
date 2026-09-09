use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as SyncMutex};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::backend::{ExecutionResult, Language, SandboxBackend, SandboxConfig};
use super::docker::DockerSandbox;
use super::monty::MontySandbox;

/// The containers one manager owns, keyed by language.
type SandboxMap = Arc<Mutex<HashMap<Language, Box<dyn SandboxBackend>>>>;

/// Every sandbox map in this process that still has containers to tear down.
///
/// A manager's `Drop` cannot await, so it can only *spawn* a cleanup task —
/// and detached tasks die with the Tokio runtime, which at app quit is
/// dropped right after `app.run` returns. A manager dropped that late would
/// leave its container running with nothing left pointing at it.
///
/// Entries are therefore held here by strong reference from construction
/// until their cleanup actually completes, so [`shutdown_all`] can finish the
/// job synchronously on the way out. Destroying twice is harmless:
/// [`destroy_all`] drains the map under its lock, so whichever call gets
/// there first takes the backends and the other finds nothing.
static LIVE_SANDBOXES: LazyLock<SyncMutex<HashMap<u64, SandboxMap>>> =
    LazyLock::new(|| SyncMutex::new(HashMap::new()));

static NEXT_MANAGER_ID: AtomicU64 = AtomicU64::new(0);

/// Track `sandboxes` in [`LIVE_SANDBOXES`], returning its key.
fn register(sandboxes: &SandboxMap) -> u64 {
    let id = NEXT_MANAGER_ID.fetch_add(1, Ordering::Relaxed);
    live_sandboxes().insert(id, sandboxes.clone());
    id
}

/// Stop tracking the map registered under `id`.
fn unregister(id: u64) {
    live_sandboxes().remove(&id);
}

/// Lock the registry, recovering from a panic in another holder.
///
/// The registry is a plain `HashMap` with no invariant a panic could leave
/// half-applied, so poisoning carries no information worth propagating —
/// and refusing to clean up containers because an unrelated task panicked
/// would be strictly worse.
fn live_sandboxes() -> std::sync::MutexGuard<'static, HashMap<u64, SandboxMap>> {
    LIVE_SANDBOXES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Destroy every sandbox container still alive in this process, awaiting
/// completion.
///
/// Call once on the way out, after the last thing that could be holding a
/// [`SandboxManager`] is gone but while a Tokio runtime is still available
/// to block on (see `chatty-gpui/src/main.rs`). Containers are started with
/// `sleep infinity` and no `--rm`, so anything missed here outlives the
/// process.
pub async fn shutdown_all() -> Result<()> {
    let maps: Vec<SandboxMap> = live_sandboxes().drain().map(|(_, map)| map).collect();

    let mut errors = Vec::new();
    for map in maps {
        if let Err(e) = destroy_all(&map).await {
            errors.push(e.to_string());
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{}", errors.join("; "))
    }
}

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
    sandboxes: SandboxMap,
    config: SandboxConfig,
    /// Key into [`LIVE_SANDBOXES`], cleared once this manager's containers
    /// have been destroyed.
    id: u64,
}

impl SandboxManager {
    pub fn new(config: SandboxConfig) -> Self {
        let sandboxes: SandboxMap = Arc::new(Mutex::new(HashMap::new()));
        let id = register(&sandboxes);
        Self {
            sandboxes,
            config,
            id,
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
///
/// Every backend is destroyed even if an earlier one fails, and failures are
/// reported together at the end. `drain()` removes an entry whether or not
/// its `destroy()` succeeds and `DockerSandbox` has no `Drop`, so returning
/// early would drop the untried backends on the floor — leaking their
/// containers *and* untracking them, leaving nothing able to retry.
async fn destroy_all(sandboxes: &Mutex<HashMap<Language, Box<dyn SandboxBackend>>>) -> Result<()> {
    let mut guard = sandboxes.lock().await;

    let mut errors = Vec::new();
    for (language, sandbox) in guard.drain() {
        if let Err(e) = sandbox.destroy().await {
            errors.push(format!("{language:?}: {e}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "failed to destroy {} sandbox container(s): {}",
            errors.len(),
            errors.join("; ")
        )
    }
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
        let id = self.id;
        let sandboxes = self.sandboxes.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // Nothing to spawn on. Leave the map registered so `shutdown_all`
            // can still reach these containers.
            warn!("no Tokio runtime available to destroy sandbox containers on drop");
            return;
        };
        handle.spawn(async move {
            if let Err(e) = destroy_all(&sandboxes).await {
                warn!(error = %e, "failed to destroy sandbox container on drop");
            }
            // Only now, once the containers are actually gone (or gone from
            // the map with the failure logged), stop tracking them. If this
            // task is cancelled before reaching here, `shutdown_all` still
            // has the map.
            unregister(id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::backend::ExecutionResult;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A fake backend that just records whether `destroy()` ran, so these
    /// tests don't need a real Docker daemon. `fail` makes `destroy()`
    /// report an error *after* recording the attempt, standing in for a
    /// Docker removal that the daemon rejects.
    struct MockBackend {
        destroyed: Arc<AtomicBool>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl SandboxBackend for MockBackend {
        async fn execute(&self, _code: &str, _language: &Language) -> Result<ExecutionResult> {
            unimplemented!("not exercised by these tests")
        }

        async fn destroy(self: Box<Self>) -> Result<()> {
            self.destroyed.store(true, Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("simulated docker removal failure");
            }
            Ok(())
        }

        fn has_port_exposed(&self, _port: u16) -> bool {
            false
        }

        async fn is_available(_docker_host: Option<&str>) -> Result<bool> {
            Ok(true)
        }
    }

    fn mock(destroyed: &Arc<AtomicBool>, fail: bool) -> Box<dyn SandboxBackend> {
        Box::new(MockBackend {
            destroyed: destroyed.clone(),
            fail,
        })
    }

    fn manager_with_mock(destroyed: Arc<AtomicBool>) -> SandboxManager {
        let manager = SandboxManager::new(SandboxConfig::default());
        manager
            .sandboxes
            .try_lock()
            .expect("uncontended in test setup")
            .insert(Language::Python, mock(&destroyed, false));
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

    /// A failing `destroy()` must not strand the containers behind it: they
    /// are removed from the map either way, so anything skipped would leak
    /// with nothing left tracking it.
    #[tokio::test]
    async fn destroy_all_destroys_every_container_even_when_one_fails() {
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let third = Arc::new(AtomicBool::new(false));

        // A bare map rather than a manager: this one is deliberately not in
        // the process registry, so its failing backend can't affect the
        // `shutdown_all` test.
        let sandboxes = Mutex::new(HashMap::new());
        {
            let mut guard = sandboxes.lock().await;
            guard.insert(Language::Python, mock(&first, true));
            guard.insert(Language::JavaScript, mock(&second, false));
            guard.insert(Language::Bash, mock(&third, false));
        }

        let err = destroy_all(&sandboxes)
            .await
            .expect_err("the failing backend should be reported");

        assert!(first.load(Ordering::SeqCst));
        assert!(
            second.load(Ordering::SeqCst) && third.load(Ordering::SeqCst),
            "a failure on one container must not skip the others"
        );
        assert!(sandboxes.lock().await.is_empty());
        assert!(
            err.to_string().contains("simulated docker removal failure"),
            "the underlying error should survive into the summary: {err}"
        );
    }

    /// The process-exit backstop: containers are reachable through the
    /// registry without going through the owning manager, which at quit may
    /// never be dropped early enough for its detached cleanup task to run.
    #[tokio::test]
    async fn shutdown_all_destroys_containers_of_a_still_live_manager() {
        let destroyed = Arc::new(AtomicBool::new(false));
        let manager = manager_with_mock(destroyed.clone());

        shutdown_all().await.expect("shutdown should succeed");

        assert!(
            destroyed.load(Ordering::SeqCst),
            "shutdown_all should reach containers via the registry"
        );
        assert!(manager.sandboxes.lock().await.is_empty());
    }
}
