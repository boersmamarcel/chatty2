//! Typed errors for the browser control layer.
//!
//! Every failure the agent can hit must land in one of these variants. A browser
//! that dies mid-task surfaces as `Crashed`, never as a hung tool call.

/// Errors produced by the CDP session layer and the browser tools.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// No usable Chrome binary, or provisioning one failed.
    #[error("browser unavailable: {0}")]
    Provisioning(String),

    /// The browser process failed to launch or the CDP handshake failed.
    #[error("failed to launch browser: {0}")]
    Launch(String),

    /// The browser process died while we were driving it.
    #[error("browser crashed: {0}")]
    Crashed(String),

    /// A CDP call did not return within its deadline.
    #[error("browser operation timed out after {0}s: {1}")]
    Timeout(u64, String),

    /// The requested navigation is outside the profile's policy.
    #[error("navigation refused: {0}")]
    NavigationRefused(String),

    /// An element ref came from a snapshot taken before the page moved.
    #[error(
        "stale element ref {0} (snapshot generation {1}, current {2}); take a new snapshot first"
    )]
    StaleRef(String, u64, u64),

    /// A CDP command returned an error, or returned something we could not read.
    #[error("browser protocol error: {0}")]
    Protocol(String),

    /// The tool needs a workspace directory to write screenshots and dumps into.
    #[error("browser tools require a workspace directory to be configured in Settings")]
    NoWorkspace,

    /// Writing a screenshot or a console/network dump failed.
    #[error("browser io error: {0}")]
    Io(String),
}

impl From<BrowserError> for crate::tools::ToolError {
    fn from(e: BrowserError) -> Self {
        crate::tools::ToolError::OperationFailed(e.to_string())
    }
}
