use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod wry_backend;

// ── Core types ───────────────────────────────────────────────────────────────

/// Unique identifier for a browser tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TabId(pub String);

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A browser cookie.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    /// Expiry as a UNIX timestamp, if any.
    #[serde(default)]
    pub expires: Option<f64>,
}

/// Summary information about an open tab.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: TabId,
    pub url: String,
    pub title: String,
}

// ── Backend trait ────────────────────────────────────────────────────────────

/// Abstraction over a browser engine (wry, chromiumoxide, Verso, etc.).
///
/// `evaluate_js` is the workhorse — click, fill, extract are all JS snippets
/// built on top of it in [`BrowserSession`](crate::session::BrowserSession).
/// This keeps the trait thin and makes adding new backends trivial.
#[async_trait]
pub trait BrowserBackend: Send + Sync + 'static {
    /// Open a new tab and return its unique identifier.
    async fn new_tab(&self) -> anyhow::Result<TabId>;

    /// Close a tab by its identifier.
    async fn close_tab(&self, tab: &TabId) -> anyhow::Result<()>;

    /// Navigate a tab to the given URL.
    async fn navigate(&self, tab: &TabId, url: &str) -> anyhow::Result<()>;

    /// Return the current URL of a tab.
    async fn current_url(&self, tab: &TabId) -> anyhow::Result<String>;

    /// Evaluate a JavaScript expression in a tab and return the result as a
    /// JSON-encoded string.
    async fn evaluate_js(&self, tab: &TabId, script: &str) -> anyhow::Result<String>;

    /// Retrieve all cookies visible to a tab.
    async fn get_cookies(&self, tab: &TabId) -> anyhow::Result<Vec<Cookie>>;

    /// Inject cookies into a tab's context.
    async fn set_cookies(&self, tab: &TabId, cookies: &[Cookie]) -> anyhow::Result<()>;

    /// Capture a PNG screenshot of a tab's viewport.
    async fn screenshot(&self, tab: &TabId) -> anyhow::Result<Vec<u8>>;

    /// Wait for a tab to finish loading (up to `timeout_ms` milliseconds).
    async fn wait_for_load(&self, tab: &TabId, timeout_ms: u64) -> anyhow::Result<()>;

    /// List all currently open tabs.
    fn list_tabs(&self) -> Vec<TabInfo>;

    /// Shut down the browser engine, closing all tabs.
    async fn shutdown(&self) -> anyhow::Result<()>;
}
