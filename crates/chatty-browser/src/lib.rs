//! `chatty-browser` — browser integration for chatty's agent.
//!
//! Provides tools for navigating JS-heavy websites, interacting with forms,
//! authenticating to services, and extracting structured data. Credentials
//! are stored in the OS keyring and never exposed to the LLM.
//!
//! # Architecture
//!
//! ```text
//! chatty-gpui ──► chatty-browser ──► chatty-core
//! chatty-tui  ──► chatty-browser ──► chatty-core
//! ```
//!
//! The [`BrowserBackend`](backend::BrowserBackend) trait abstracts over the
//! browser engine. The default implementation uses wry/tao for a real
//! OS-native WebView, with an HTTP fallback for headless environments.
//!
//! All DOM interaction is performed via JavaScript snippets in
//! [`BrowserSession`](session::BrowserSession), making the backend trait thin.

use std::sync::Arc;

pub mod backend;
pub mod constants;
pub mod credential;
pub mod http_fallback;
pub mod page;
pub mod session;
pub mod settings;
pub mod tools;
pub mod utils;

// ── GPUI integration (optional feature) ──────────────────────────────────────
#[cfg(feature = "gpui-globals")]
mod gpui_globals;

// ── Re-exports ───────────────────────────────────────────────────────────────
pub use backend::{BrowserBackend, Cookie, TabId, TabInfo};
pub use page::PageSnapshot;
pub use session::{BrowserSession, SharedCookieJar, validate_url_scheme};
pub use settings::BrowserSettingsModel;

/// Singleton browser engine.
///
/// Manages the backend lifecycle and provides shared access to the
/// [`BrowserSession`]. Lazily initialized on first use.
pub struct BrowserEngine {
    session: Arc<BrowserSession>,
    active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
}

impl BrowserEngine {
    /// Create a new browser engine with the given backend.
    pub fn new(backend: Arc<dyn BrowserBackend>) -> Self {
        let session = Arc::new(BrowserSession::new(backend));
        Self {
            session,
            active_tab: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    /// Get a reference to the shared session.
    pub fn session(&self) -> &Arc<BrowserSession> {
        &self.session
    }

    /// Get a reference to the active tab lock.
    pub fn active_tab(&self) -> &Arc<tokio::sync::RwLock<Option<TabId>>> {
        &self.active_tab
    }

    /// Shut down the browser engine.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.session.backend().shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::wry_backend::WryBackend;

    #[tokio::test]
    async fn test_browser_engine_creation() {
        // WryBackend::new() requires a display server (X11/Wayland/macOS/Windows).
        // In headless CI it may fail — that's expected. Only test when it succeeds.
        match WryBackend::new().await {
            Ok(backend) => {
                let engine = BrowserEngine::new(Arc::new(backend));
                assert!(engine.session().backend().list_tabs().is_empty());
            }
            Err(e) => {
                eprintln!("WryBackend::new() failed (expected in headless CI): {e}");
            }
        }
    }
}
