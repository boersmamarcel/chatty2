/// Errors specific to the chatty-browser crate.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error(
        "Versoview binary not found at '{0}'. Install Verso or set the path in Settings → Browser."
    )]
    VersoviewNotFound(String),

    #[error("Failed to spawn versoview process: {0}")]
    SpawnFailed(String),

    #[error("Versoview process exited unexpectedly: {0}")]
    ProcessExited(String),

    #[error("DevTools connection failed: {0}")]
    DevToolsConnectionFailed(String),

    #[error("DevTools protocol error: {0}")]
    DevToolsProtocol(String),

    #[error("Navigation failed: {0}")]
    NavigationFailed(String),

    #[error("Page load timed out after {0}ms")]
    PageLoadTimeout(u64),

    #[error("JavaScript evaluation error: {0}")]
    JsEvalError(String),

    #[error("Session '{0}' not found")]
    SessionNotFound(String),

    #[error("Browser engine not running")]
    EngineNotRunning,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
