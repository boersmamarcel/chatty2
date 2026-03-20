//! Verso browser engine lifecycle management.
//!
//! `BrowserEngine` manages the `versoview` sidecar process: spawning, health
//! checking, and graceful shutdown. It owns the [`DevToolsClient`] used for
//! automation and provides methods to create [`BrowserSession`]s.

use crate::devtools::DevToolsClient;
use crate::error::BrowserError;
use crate::session::BrowserSession;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Default number of connection retries when waiting for the DevTools server.
const DEFAULT_CONNECT_RETRIES: u32 = 20;

/// Configuration for the browser engine.
#[derive(Clone, Debug)]
pub struct BrowserEngineConfig {
    /// Path to the `versoview` binary. Auto-detected if `None`.
    pub versoview_path: Option<PathBuf>,
    /// DevTools server port. `0` means pick a random available port.
    pub devtools_port: u16,
    /// Run in headless mode (no visible window).
    pub headless: bool,
    /// Page load timeout in milliseconds.
    pub page_load_timeout_ms: u64,
    /// Mock mode: return realistic fake page data without launching versoview.
    /// Useful for testing the full tool pipeline and LLM integration.
    /// Enable via `BrowserEngineConfig { mock_mode: true, .. }` or by setting
    /// the `CHATTY_BROWSER_MOCK=1` environment variable.
    pub mock_mode: bool,
}

impl Default for BrowserEngineConfig {
    fn default() -> Self {
        Self {
            versoview_path: None,
            devtools_port: 0,
            headless: false,
            page_load_timeout_ms: 30_000,
            mock_mode: false,
        }
    }
}

/// Manages the versoview sidecar process and DevTools connection.
pub struct BrowserEngine {
    config: BrowserEngineConfig,
    /// The versoview child process (if running).
    process: Mutex<Option<Child>>,
    /// DevTools client for automation commands.
    devtools: Arc<DevToolsClient>,
    /// The actual port the DevTools server is running on (resolved from config).
    resolved_port: u16,
    /// Next session ID counter.
    next_session_id: std::sync::atomic::AtomicU64,
    /// Whether mock mode is active (no real browser process).
    mock_running: std::sync::atomic::AtomicBool,
}

impl BrowserEngine {
    /// Create a new `BrowserEngine` with the given configuration.
    ///
    /// Does not start the process — call [`start`] to launch versoview.
    pub fn new(config: BrowserEngineConfig) -> Self {
        let port = if config.devtools_port == 0 {
            pick_available_port()
        } else {
            config.devtools_port
        };

        Self {
            config,
            process: Mutex::new(None),
            devtools: Arc::new(DevToolsClient::new(port)),
            resolved_port: port,
            next_session_id: std::sync::atomic::AtomicU64::new(1),
            mock_running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Start the versoview process and connect to its DevTools server.
    ///
    /// In mock mode, this succeeds immediately without launching any process.
    pub async fn start(&self) -> Result<(), BrowserError> {
        if self.config.mock_mode {
            info!("Browser engine starting in MOCK mode (no versoview process)");
            self.mock_running
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }

        let binary_path = self.resolve_versoview_path()?;
        info!(path = %binary_path.display(), port = self.resolved_port, "Starting versoview");

        let mut cmd = Command::new(&binary_path);
        cmd.arg("--devtools-port")
            .arg(self.resolved_port.to_string());

        if self.config.headless {
            cmd.arg("--headless");
        }

        // Suppress stdout/stderr from the child process
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let child = cmd.spawn().map_err(|e| {
            BrowserError::SpawnFailed(format!("Failed to start {}: {}", binary_path.display(), e))
        })?;

        {
            let mut proc = self.process.lock().await;
            *proc = Some(child);
        }

        // Connect to the DevTools server with retries
        self.devtools.connect(DEFAULT_CONNECT_RETRIES).await?;

        info!("Versoview started and DevTools connected");
        Ok(())
    }

    /// Stop the versoview process gracefully.
    pub async fn stop(&self) {
        // Disconnect DevTools first
        self.devtools.disconnect().await;

        let mut proc = self.process.lock().await;
        if let Some(mut child) = proc.take() {
            debug!("Stopping versoview process");
            // Try graceful kill first
            if let Err(e) = child.kill().await {
                warn!(error = %e, "Failed to kill versoview process");
            }
            // Wait for the process to exit
            match child.wait().await {
                Ok(status) => debug!(?status, "Versoview process exited"),
                Err(e) => warn!(error = %e, "Error waiting for versoview exit"),
            }
        }
    }

    /// Check if the engine is currently running and connected.
    pub async fn is_running(&self) -> bool {
        if self.mock_running.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        let proc = self.process.lock().await;
        if proc.is_none() {
            return false;
        }
        self.devtools.is_connected().await
    }

    /// Create a new browser session (one tab/context).
    pub fn create_session(&self) -> BrowserSession {
        let session_id = self
            .next_session_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BrowserSession::new(
            format!("session-{}", session_id),
            self.devtools.clone(),
            self.config.page_load_timeout_ms,
            self.config.mock_mode,
        )
    }

    /// Access the DevTools client directly.
    pub fn devtools(&self) -> &Arc<DevToolsClient> {
        &self.devtools
    }

    /// Return the resolved DevTools port.
    pub fn port(&self) -> u16 {
        self.resolved_port
    }

    /// Check if the engine is configured in mock mode.
    pub fn is_mock(&self) -> bool {
        self.config.mock_mode
    }

    /// Resolve the path to the `versoview` binary.
    ///
    /// Search order:
    /// 1. Explicit path from config
    /// 2. `VERSOVIEW_PATH` environment variable
    /// 3. `versoview` on `$PATH`
    fn resolve_versoview_path(&self) -> Result<PathBuf, BrowserError> {
        // 1. Explicit config path
        if let Some(ref path) = self.config.versoview_path {
            if path.exists() {
                return Ok(path.clone());
            }
            return Err(BrowserError::VersoviewNotFound(path.display().to_string()));
        }

        // 2. Environment variable
        if let Ok(env_path) = std::env::var("VERSOVIEW_PATH") {
            let path = PathBuf::from(&env_path);
            if path.exists() {
                return Ok(path);
            }
            warn!(
                path = %env_path,
                "VERSOVIEW_PATH set but binary not found at path"
            );
        }

        // 3. Search $PATH
        if let Ok(which_path) = which_versoview() {
            return Ok(which_path);
        }

        Err(BrowserError::VersoviewNotFound(
            "versoview (not found on PATH, in VERSOVIEW_PATH, or in config)".to_string(),
        ))
    }
}

impl Drop for BrowserEngine {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup: try to kill the child process.
        // The async `stop()` method should be preferred for graceful shutdown.
        if let Ok(mut proc) = self.process.try_lock()
            && let Some(ref mut child) = *proc
        {
            // start_kill is sync and non-blocking
            let _ = child.start_kill();
        }
    }
}

/// Pick a random available TCP port by binding to port 0.
fn pick_available_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|addr| addr.port())
        .unwrap_or(6080) // Fallback if binding fails; 6080 is Verso's conventional DevTools port
}

/// Search for `versoview` on `$PATH`.
fn which_versoview() -> Result<PathBuf, BrowserError> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let binary_name = if cfg!(windows) {
        "versoview.exe"
    } else {
        "versoview"
    };

    for dir in path_var.split(separator) {
        let candidate = PathBuf::from(dir).join(binary_name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(BrowserError::VersoviewNotFound(binary_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pick_available_port() {
        let port = pick_available_port();
        assert!(port > 0);
    }

    #[test]
    fn test_default_config() {
        let config = BrowserEngineConfig::default();
        assert_eq!(config.devtools_port, 0);
        assert!(!config.headless);
        assert!(!config.mock_mode);
        assert_eq!(config.page_load_timeout_ms, 30_000);
        assert!(config.versoview_path.is_none());
    }

    #[test]
    fn test_engine_creation() {
        let engine = BrowserEngine::new(BrowserEngineConfig::default());
        // Port should be resolved (non-zero since 0 triggers auto-pick)
        assert!(engine.port() > 0);
    }

    #[test]
    fn test_session_id_incrementing() {
        let engine = BrowserEngine::new(BrowserEngineConfig::default());
        let s1 = engine.create_session();
        let s2 = engine.create_session();
        assert_ne!(s1.id(), s2.id());
    }
}
