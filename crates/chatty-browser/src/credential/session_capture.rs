//! Session capture — opens a visible browser window for manual login.
//!
//! The user logs in manually (handling 2FA, CAPTCHA, etc.), then clicks
//! "Done" in chatty's UI. Chatty captures all cookies and stores them
//! in the credential vault.
//!
//! This module is only functional in chatty-gpui (requires a visible window).
//! chatty-tui can *use* captured sessions but cannot *create* them.

use crate::backend::{BrowserBackend, Cookie, TabId};
use crate::credential::types::LoginSecret;
use crate::credential::vault::CredentialVault;
use std::sync::Arc;

/// Capture session cookies from a visible browser window.
///
/// # Flow
/// 1. Backend opens a visible tab navigated to `url`.
/// 2. The user logs in manually.
/// 3. Caller invokes `capture_and_store` to grab all cookies and save them.
pub struct SessionCapture {
    backend: Arc<dyn BrowserBackend>,
    vault: CredentialVault,
}

impl SessionCapture {
    pub fn new(backend: Arc<dyn BrowserBackend>, vault: CredentialVault) -> Self {
        Self { backend, vault }
    }

    /// Open a tab to the given URL for manual login.
    ///
    /// Returns the `TabId` so the caller can later call `capture_and_store`.
    pub async fn open_login_page(&self, url: &str) -> anyhow::Result<TabId> {
        crate::session::validate_url_scheme(url)?;
        let tab = self.backend.new_tab().await?;
        self.backend.navigate(&tab, url).await?;
        Ok(tab)
    }

    /// Capture cookies from the tab and store them under `credential_name`.
    pub async fn capture_and_store(
        &self,
        tab: &TabId,
        credential_name: &str,
    ) -> anyhow::Result<Vec<Cookie>> {
        let cookies = self.backend.get_cookies(tab).await?;

        if cookies.is_empty() {
            tracing::warn!(
                credential = credential_name,
                "No cookies captured — login may not have completed"
            );
        } else {
            tracing::info!(
                credential = credential_name,
                cookie_count = cookies.len(),
                "Session cookies captured"
            );
        }

        let secret = LoginSecret::SessionCookies(cookies.clone());
        self.vault.store(credential_name, &secret).await?;

        Ok(cookies)
    }

    /// Close the login tab after capture.
    pub async fn close_tab(&self, tab: &TabId) -> anyhow::Result<()> {
        self.backend.close_tab(tab).await
    }
}
