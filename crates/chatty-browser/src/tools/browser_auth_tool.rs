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
                     Please add it in Settings > Browser Credentials."
                ))
            })?;

        // Load the secret from vault
        let secret = self.vault.load(name).await.map_err(|e| {
            tracing::warn!(
                credential = name,
                error = ?e,
                "Failed to load secret from vault"
            );
            let hint = match profile.auth_method {
                AuthMethod::SessionCapture => {
                    "Session cookies have not been captured yet for this profile. \
                     Please complete the session capture flow in Settings > Browser Credentials."
                }
                AuthMethod::FormLogin => {
                    "Login credentials (username/password) are missing for this profile. \
                     Please re-add the credential with username and password in \
                     Settings > Browser Credentials."
                }
            };
            BrowserAuthError::CredentialNotFound(format!(
                "Profile \"{name}\" exists but has no stored secret. {hint}"
            ))
        })?;

        // Try the full browser backend first
        match self.try_backend_auth(&profile, &secret, name).await {
            Ok(output) => Ok(output),
            Err(backend_err) => {
                // Fall back to HTTP-based authentication
                tracing::debug!(
                    credential = name,
                    error = %backend_err,
                    "Browser backend unavailable, falling back to HTTP auth"
                );
                self.try_http_auth(&profile, &secret, name).await
            }
        }
    }
}

impl BrowserAuthTool {
    /// Try authentication using the full browser backend (WebView).
    async fn try_backend_auth(
        &self,
        profile: &crate::credential::types::LoginProfile,
        secret: &LoginSecret,
        name: &str,
    ) -> Result<BrowserAuthOutput, BrowserAuthError> {
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

        match (&profile.auth_method, secret) {
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

                // Small delay for JS frameworks to hydrate the page
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

                // Fill username using nativeInputValueSetter for React/Vue/Angular compatibility
                if let Some(user_sel) = &profile.username_selector {
                    let js = format!(
                        r#"(() => {{
                            const el = document.querySelector("{}");
                            if (!el) return JSON.stringify({{ error: "Username field not found: {}" }});
                            const setter = Object.getOwnPropertyDescriptor(
                                window.HTMLInputElement.prototype, 'value'
                            )?.set || Object.getOwnPropertyDescriptor(
                                window.HTMLTextAreaElement.prototype, 'value'
                            )?.set;
                            if (setter) {{
                                setter.call(el, "{}");
                            }} else {{
                                el.value = "{}";
                            }}
                            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                            el.dispatchEvent(new Event('blur', {{ bubbles: true }}));
                            return JSON.stringify({{ success: true }});
                        }})()"#,
                        crate::session::escape_js_string(user_sel),
                        crate::session::escape_js_string(user_sel),
                        crate::session::escape_js_string(username),
                        crate::session::escape_js_string(username)
                    );
                    let result = self
                        .session
                        .backend()
                        .evaluate_js(&tab, &js)
                        .await
                        .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
                    Self::check_js_result(&result, "username")?;
                }

                // Brief pause between fields
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;

                // Fill password
                if let Some(pass_sel) = &profile.password_selector {
                    let js = format!(
                        r#"(() => {{
                            const el = document.querySelector("{}");
                            if (!el) return JSON.stringify({{ error: "Password field not found: {}" }});
                            const setter = Object.getOwnPropertyDescriptor(
                                window.HTMLInputElement.prototype, 'value'
                            )?.set || Object.getOwnPropertyDescriptor(
                                window.HTMLTextAreaElement.prototype, 'value'
                            )?.set;
                            if (setter) {{
                                setter.call(el, "{}");
                            }} else {{
                                el.value = "{}";
                            }}
                            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                            el.dispatchEvent(new Event('blur', {{ bubbles: true }}));
                            return JSON.stringify({{ success: true }});
                        }})()"#,
                        crate::session::escape_js_string(pass_sel),
                        crate::session::escape_js_string(pass_sel),
                        crate::session::escape_js_string(password),
                        crate::session::escape_js_string(password)
                    );
                    let result = self
                        .session
                        .backend()
                        .evaluate_js(&tab, &js)
                        .await
                        .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
                    Self::check_js_result(&result, "password")?;
                }

                // Brief pause before submit
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;

                // Capture the URL before submitting so we can detect navigation
                let url_before = self
                    .session
                    .backend()
                    .current_url(&tab)
                    .await
                    .unwrap_or_default();

                // Submit
                if let Some(submit_sel) = &profile.submit_selector {
                    let js = format!(
                        r#"(() => {{
                            const el = document.querySelector("{}");
                            if (!el) return JSON.stringify({{ error: "Submit button not found: {}" }});
                            el.click();
                            return JSON.stringify({{ success: true }});
                        }})()"#,
                        crate::session::escape_js_string(submit_sel),
                        crate::session::escape_js_string(submit_sel),
                    );
                    let result = self
                        .session
                        .backend()
                        .evaluate_js(&tab, &js)
                        .await
                        .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;
                    Self::check_js_result(&result, "submit")?;
                }

                // Wait for navigation after login
                self.session
                    .backend()
                    .wait_for_load(&tab, 15_000)
                    .await
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

                // Verify login by checking URL changed or page doesn't have password field
                let url_after = self
                    .session
                    .backend()
                    .current_url(&tab)
                    .await
                    .unwrap_or_default();

                if url_after == url_before {
                    tracing::warn!(
                        url = %url_after,
                        "URL did not change after login submit — login may have failed"
                    );
                } else {
                    tracing::info!(
                        before = %url_before,
                        after = %url_after,
                        "Login navigated to new URL"
                    );
                }
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

    /// Check a JS evaluation result for errors returned by our form-fill snippets.
    fn check_js_result(result: &str, field_name: &str) -> Result<(), BrowserAuthError> {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(result) {
            if let Some(error) = json.get("error").and_then(|v| v.as_str()) {
                tracing::warn!(field = field_name, error = error, "Form field JS error");
                return Err(BrowserAuthError::AuthFailed(format!(
                    "Failed to fill {field_name}: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Fallback: authenticate via HTTP using reqwest.
    ///
    /// For **form_login** profiles, POSTs credentials to the login URL and
    /// stores response cookies in the session's shared cookie jar. Subsequent
    /// `browse` calls use that jar so pages are fetched authenticated.
    ///
    /// For **session_capture** profiles with stored cookies, injects them into
    /// the shared jar directly.
    async fn try_http_auth(
        &self,
        profile: &crate::credential::types::LoginProfile,
        secret: &LoginSecret,
        name: &str,
    ) -> Result<BrowserAuthOutput, BrowserAuthError> {
        let jar = self.session.cookie_jar().clone();

        match (&profile.auth_method, secret) {
            (AuthMethod::SessionCapture, LoginSecret::SessionCookies(cookies)) => {
                // Inject stored cookies into the shared jar
                let url = url::Url::parse(&profile.url_pattern).map_err(|e| {
                    BrowserAuthError::AuthFailed(format!("Invalid URL pattern: {e}"))
                })?;
                for cookie in cookies {
                    let cookie_str = format!("{}={}", cookie.name, cookie.value);
                    jar.add_cookie_str(&cookie_str, &url);
                }
                tracing::info!(
                    credential = name,
                    cookies = cookies.len(),
                    "Injected session cookies into HTTP cookie jar"
                );
                Ok(BrowserAuthOutput {
                    success: true,
                    url: profile.url_pattern.clone(),
                    message: format!(
                        "Authenticated as \"{name}\" (session cookies injected into HTTP client)"
                    ),
                })
            }
            (AuthMethod::FormLogin, LoginSecret::FormCredentials { username, password }) => {
                // POST credentials via HTTP
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .user_agent(crate::http_fallback::BROWSER_USER_AGENT)
                    .default_headers({
                        let mut headers = reqwest::header::HeaderMap::new();
                        headers.insert(reqwest::header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8".parse().unwrap());
                        headers.insert(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9".parse().unwrap());
                        headers.insert(reqwest::header::ACCEPT_ENCODING, "gzip, deflate, br".parse().unwrap());
                        headers.insert("Sec-Fetch-Dest", "document".parse().unwrap());
                        headers.insert("Sec-Fetch-Mode", "navigate".parse().unwrap());
                        headers.insert("Sec-Fetch-Site", "none".parse().unwrap());
                        headers.insert("Sec-Fetch-User", "?1".parse().unwrap());
                        headers.insert("Upgrade-Insecure-Requests", "1".parse().unwrap());
                        headers
                    })
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .cookie_provider(jar.clone())
                    .build()
                    .map_err(|e| BrowserAuthError::AuthFailed(e.to_string()))?;

                // First fetch the login page to get any CSRF tokens/cookies
                let login_url = &profile.url_pattern;
                let _get_resp = client.get(login_url).send().await.map_err(|e| {
                    BrowserAuthError::AuthFailed(format!("Failed to load login page: {e}"))
                })?;

                // Determine form field names from selectors (best-effort extraction).
                // Common defaults used as fallback: most login forms use "email" and
                // "password" as field names. Sites with non-standard names (e.g.
                // "login", "user_name") should have CSS selectors with name= attributes
                // configured in their credential profile.
                let username_field =
                    Self::selector_to_field_name(profile.username_selector.as_deref())
                        .unwrap_or("email");
                let password_field =
                    Self::selector_to_field_name(profile.password_selector.as_deref())
                        .unwrap_or("password");

                // POST the login form
                let form_data = [
                    (username_field.to_string(), username.clone()),
                    (password_field.to_string(), password.clone()),
                ];

                let response = client
                    .post(login_url)
                    .form(&form_data)
                    .send()
                    .await
                    .map_err(|e| {
                        BrowserAuthError::AuthFailed(format!("HTTP form login failed: {e}"))
                    })?;

                let final_url = response.url().to_string();
                let status = response.status();

                tracing::info!(
                    credential = name,
                    status = %status,
                    final_url = %final_url,
                    "HTTP form login completed"
                );

                if status.is_success() || status.is_redirection() {
                    Ok(BrowserAuthOutput {
                        success: true,
                        url: final_url,
                        message: format!(
                            "Authenticated as \"{name}\" via HTTP (cookies stored for subsequent browse calls)"
                        ),
                    })
                } else {
                    Ok(BrowserAuthOutput {
                        success: false,
                        url: final_url,
                        message: format!(
                            "HTTP login returned status {status}. \
                             The site may require JavaScript or OAuth. \
                             Try using the browse tool to visit the site directly."
                        ),
                    })
                }
            }
            _ => Err(BrowserAuthError::AuthFailed(format!(
                "Auth method {:?} does not match stored secret type for \"{name}\"",
                profile.auth_method
            ))),
        }
    }

    /// Best-effort extraction of a form field name from a CSS selector.
    ///
    /// For selectors like `input[name="email"]`, extracts `"email"`.
    /// For `#username`, returns `"username"`.
    /// Falls back to `None` for complex selectors.
    fn selector_to_field_name(selector: Option<&str>) -> Option<&str> {
        let sel = selector?.trim();

        // Try to extract from name="..." pattern
        if let Some(start) = sel.find("name=\"") {
            let rest = &sel[start + 6..];
            if let Some(end) = rest.find('"') {
                return Some(&rest[..end]);
            }
        }
        if let Some(start) = sel.find("name='") {
            let rest = &sel[start + 6..];
            if let Some(end) = rest.find('\'') {
                return Some(&rest[..end]);
            }
        }

        // Try #id selector → use id as field name
        if let Some(id) = sel.strip_prefix('#') {
            // Only use simple IDs (no spaces/combinators)
            if !id.contains(' ') && !id.contains(',') {
                return Some(id);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selector_to_field_name_with_name_attr() {
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some(r#"input[name="email"]"#)),
            Some("email")
        );
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some(r#"input[name="username"]"#)),
            Some("username")
        );
    }

    #[test]
    fn test_selector_to_field_name_with_single_quotes() {
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some("input[name='password']")),
            Some("password")
        );
    }

    #[test]
    fn test_selector_to_field_name_with_id() {
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some("#email")),
            Some("email")
        );
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some("#username")),
            Some("username")
        );
    }

    #[test]
    fn test_selector_to_field_name_complex_selector_returns_none() {
        // Multiple selectors (comma-separated) → too ambiguous
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some(
                r#"input[type="email"], input[name="email"]"#
            )),
            // Finds name="email" in the second part
            Some("email")
        );
    }

    #[test]
    fn test_selector_to_field_name_type_only_returns_none() {
        // Type-only selectors don't tell us the field name
        assert_eq!(
            BrowserAuthTool::selector_to_field_name(Some(r#"input[type="password"]"#)),
            None
        );
    }

    #[test]
    fn test_selector_to_field_name_none_input() {
        assert_eq!(BrowserAuthTool::selector_to_field_name(None), None);
    }

    #[test]
    fn test_selector_to_field_name_empty() {
        assert_eq!(BrowserAuthTool::selector_to_field_name(Some("")), None);
    }
}
