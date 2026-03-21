//! `browser_auth` tool — authenticate via stored credentials.
//!
//! The LLM only sees credential **names** and auth **results**. It never
//! sees usernames, passwords, cookies, or selectors.
//!
//! Always requires approval.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend::TabId;
use crate::credential::types::{AuthMethod, LoginSecret};
use crate::credential::vault::CredentialVault;
use crate::session::BrowserSession;
use crate::settings::login_profiles::LoginProfileRepository;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BrowserAuthError {
    #[error("Auth failed: {0}")]
    AuthFailed(String),
    #[error("Credential not found: {0}")]
    CredentialNotFound(String),
}

// ── Args / Output ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct BrowserAuthArgs {
    /// The name of the stored credential to use (e.g., "komoot", "strava").
    pub credential_name: String,
}

#[derive(Debug, Serialize)]
pub struct BrowserAuthOutput {
    pub success: bool,
    /// Current URL after authentication.
    pub url: String,
    /// Human-readable status message.
    pub message: String,
}

impl std::fmt::Display for BrowserAuthOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.success {
            write!(f, "✓ Authenticated — now at {}", self.url)
        } else {
            write!(f, "✗ Auth failed: {}", self.message)
        }
    }
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Authenticate with a website using a stored credential.
///
/// Always requires approval. The LLM only provides the credential name;
/// all secrets are loaded from the OS keyring and never exposed.
#[derive(Clone)]
pub struct BrowserAuthTool {
    session: Arc<BrowserSession>,
    active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
    vault: Arc<CredentialVault>,
    profiles_repo: Arc<LoginProfileRepository>,
}

impl BrowserAuthTool {
    pub fn new(
        session: Arc<BrowserSession>,
        active_tab: Arc<tokio::sync::RwLock<Option<TabId>>>,
        vault: Arc<CredentialVault>,
        profiles_repo: Arc<LoginProfileRepository>,
    ) -> Self {
        Self {
            session,
            active_tab,
            vault,
            profiles_repo,
        }
    }
}

impl Tool for BrowserAuthTool {
    const NAME: &'static str = "browser_auth";

    type Error = BrowserAuthError;
    type Args = BrowserAuthArgs;
    type Output = BrowserAuthOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browser_auth".to_string(),
            description: "Authenticate with a website using a stored credential. \
                Provide the credential name (e.g., 'komoot', 'strava'). \
                Credentials are securely stored in the OS keyring — you will never \
                see passwords or cookies. Returns the resulting URL after auth."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "credential_name": {
                        "type": "string",
                        "description": "Name of the stored credential to use"
                    }
                },
                "required": ["credential_name"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let name = &args.credential_name;

        // Load the profile
        let profile = self
            .profiles_repo
            .find_by_name(name)
            .await
            .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?
            .ok_or_else(|| {
                BrowserAuthError::CredentialNotFound(format!(
                    "No login profile named \"{name}\" exists. \
                     Please add it in Settings → Browser Credentials."
                ))
            })?;

        // Load the secret from vault
        let secret = self.vault.load(name).await.map_err(|_| {
            let hint = match profile.auth_method {
                AuthMethod::SessionCapture => {
                    "Session cookies have not been captured yet for this profile. \
                     Please complete the session capture flow in Settings → Browser Credentials."
                }
                AuthMethod::FormLogin => {
                    "Login credentials (username/password) are missing for this profile. \
                     Please re-add the credential with username and password in \
                     Settings → Browser Credentials."
                }
            };
            BrowserAuthError::CredentialNotFound(format!(
                "Profile \"{name}\" exists but has no stored secret. {hint}"
            ))
        })?;

        // Get or create a tab
        let mut tab_guard = self.active_tab.write().await;
        let tab = if let Some(existing) = tab_guard.as_ref() {
            existing.clone()
        } else {
            let new_tab = self
                .session
                .backend()
                .new_tab()
                .await
                .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
            *tab_guard = Some(new_tab.clone());
            new_tab
        };
        drop(tab_guard);

        match (&profile.auth_method, &secret) {
            (AuthMethod::SessionCapture, LoginSecret::SessionCookies(cookies)) => {
                // Inject cookies, then navigate to the site
                self.session
                    .set_cookies(&tab, cookies)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

                self.session
                    .backend()
                    .navigate(&tab, &profile.url_pattern)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

                self.session
                    .backend()
                    .wait_for_load(&tab, 15_000)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
            }
            (AuthMethod::FormLogin, LoginSecret::FormCredentials { username, password }) => {
                // Navigate to login page
                self.session
                    .backend()
                    .navigate(&tab, &profile.url_pattern)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

                self.session
                    .backend()
                    .wait_for_load(&tab, 15_000)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

                // Fill form using selectors
                if let Some(user_sel) = &profile.username_selector {
                    let js = format!(
                        r#"(() => {{
                            const el = document.querySelector("{}");
                            if (!el) return JSON.stringify({{ error: "Username field not found" }});
                            el.value = "{}";
                            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            return JSON.stringify({{ success: true }});
                        }})()"#,
                        crate::session::escape_js_string(user_sel),
                        crate::session::escape_js_string(username)
                    );
                    self.session
                        .backend()
                        .evaluate_js(&tab, &js)
                        .await
                        .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
                }

                if let Some(pass_sel) = &profile.password_selector {
                    let js = format!(
                        r#"(() => {{
                            const el = document.querySelector("{}");
                            if (!el) return JSON.stringify({{ error: "Password field not found" }});
                            el.value = "{}";
                            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            return JSON.stringify({{ success: true }});
                        }})()"#,
                        crate::session::escape_js_string(pass_sel),
                        crate::session::escape_js_string(password)
                    );
                    self.session
                        .backend()
                        .evaluate_js(&tab, &js)
                        .await
                        .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
                }

                // Submit
                if let Some(submit_sel) = &profile.submit_selector {
                    let js = format!(
                        r#"(() => {{
                            const el = document.querySelector("{}");
                            if (!el) return JSON.stringify({{ error: "Submit button not found" }});
                            el.click();
                            return JSON.stringify({{ success: true }});
                        }})()"#,
                        crate::session::escape_js_string(submit_sel),
                    );
                    self.session
                        .backend()
                        .evaluate_js(&tab, &js)
                        .await
                        .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
                }

                // Wait for navigation after login
                self.session
                    .backend()
                    .wait_for_load(&tab, 15_000)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
            }
            _ => {
                return Err(BrowserAuthError::AuthFailed(format!(
                    "Auth method {:?} does not match stored secret type for \"{name}\"",
                    profile.auth_method
                )));
            }
        }

        // Get the current URL after auth
        let current_url = self
            .session
            .backend()
            .current_url(&tab)
            .await
            .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

        Ok(BrowserAuthOutput {
            success: true,
            url: current_url,
            message: format!("Authenticated as \"{name}\""),
        })
    }
}
