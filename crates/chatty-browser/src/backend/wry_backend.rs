//! Stub implementation of the wry/tao browser backend.
//!
//! The actual wry integration requires system-level WebView libraries
//! (WebKitGTK on Linux, WebKit on macOS, WebView2 on Windows).
//!
//! This module provides the `WryBackend` type that will, in a future phase,
//! run tao's event loop on a dedicated thread and communicate via
//! `EventLoopProxy` + `oneshot` channels (the same pattern Tauri uses).
//!
//! For now it returns clear errors indicating the engine is not yet connected.

use async_trait::async_trait;

use super::{BrowserBackend, Cookie, TabId, TabInfo};

/// Wry/tao-based browser backend.
///
/// Manages an OS-native WebView via wry, with tao providing the windowing
/// layer. The event loop runs on a dedicated thread; all async methods
/// communicate with it via channels.
#[derive(Default)]
pub struct WryBackend {
    _private: (),
}

impl WryBackend {
    /// Create a new wry backend.
    ///
    /// In the full implementation this spawns the tao event loop thread.
    pub fn new() -> anyhow::Result<Self> {
        tracing::info!("WryBackend created (stub — full wry integration pending)");
        Ok(Self { _private: () })
    }
}

#[async_trait]
impl BrowserBackend for WryBackend {
    async fn new_tab(&self) -> anyhow::Result<TabId> {
        anyhow::bail!("WryBackend: new_tab not yet implemented — wry integration pending")
    }

    async fn close_tab(&self, _tab: &TabId) -> anyhow::Result<()> {
        anyhow::bail!("WryBackend: close_tab not yet implemented")
    }

    async fn navigate(&self, _tab: &TabId, _url: &str) -> anyhow::Result<()> {
        anyhow::bail!("WryBackend: navigate not yet implemented")
    }

    async fn current_url(&self, _tab: &TabId) -> anyhow::Result<String> {
        anyhow::bail!("WryBackend: current_url not yet implemented")
    }

    async fn evaluate_js(&self, _tab: &TabId, _script: &str) -> anyhow::Result<String> {
        anyhow::bail!("WryBackend: evaluate_js not yet implemented")
    }

    async fn get_cookies(&self, _tab: &TabId) -> anyhow::Result<Vec<Cookie>> {
        anyhow::bail!("WryBackend: get_cookies not yet implemented")
    }

    async fn set_cookies(&self, _tab: &TabId, _cookies: &[Cookie]) -> anyhow::Result<()> {
        anyhow::bail!("WryBackend: set_cookies not yet implemented")
    }

    async fn screenshot(&self, _tab: &TabId) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("WryBackend: screenshot not yet implemented")
    }

    async fn wait_for_load(&self, _tab: &TabId, _timeout_ms: u64) -> anyhow::Result<()> {
        anyhow::bail!("WryBackend: wait_for_load not yet implemented")
    }

    fn list_tabs(&self) -> Vec<TabInfo> {
        Vec::new()
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
