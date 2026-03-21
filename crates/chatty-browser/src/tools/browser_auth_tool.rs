//! The `browser_auth` tool — navigate to a URL using stored browser credentials.
//!
//! This tool loads cookies from a named credential (captured via the session
//! capture flow in the settings UI), navigates to the target URL, injects the
//! cookies, and then reloads the page so the server sees the authenticated session.

use crate::engine::BrowserEngine;
use crate::page_repr::PageSnapshot;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Arguments for the browser_auth tool.
#[derive(Deserialize, Serialize)]
pub struct BrowserAuthToolArgs {
    /// The name of the stored credential to use (e.g., "komoot", "strava").
    pub credential_name: String,
    /// The URL to navigate to after injecting credentials.
    pub url: String,
}

/// Output from the browser_auth tool.
#[derive(Debug, Serialize)]
pub struct BrowserAuthToolOutput {
    /// Whether credentials were successfully injected.
    pub authenticated: bool,
    /// Page title after authentication.
    pub title: String,
    /// Current URL (may differ from requested URL due to redirects).
    pub url: String,
    /// Readable text summary of the page content.
    pub content: String,
    /// Number of cookies injected.
    pub cookies_injected: usize,
    /// Human-readable page representation for the LLM.
    pub page_snapshot: String,
}

impl std::fmt::Display for BrowserAuthToolOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.authenticated {
            write!(f, "{}", self.page_snapshot)
        } else {
            write!(f, "Authentication failed for URL: {}", self.url)
        }
    }
}

/// Error type for the browser_auth tool.
#[derive(Debug, thiserror::Error)]
pub enum BrowserAuthToolError {
    #[error("Browser auth error: {0}")]
    AuthError(String),
}

/// Stored credential data passed to the tool at construction time.
/// This avoids coupling chatty-browser to chatty-core's credential store.
#[derive(Clone, Debug)]
pub struct StoredCredential {
    /// Credential name.
    pub name: String,
    /// URL pattern (e.g., "https://komoot.com/*").
    pub url_pattern: String,
    /// Cookies to inject: (name, value, domain, path).
    pub cookies: Vec<(String, String, String, String)>,
}

/// The `browser_auth` tool: navigates to a URL with stored credentials.
#[derive(Clone)]
pub struct BrowserAuthTool {
    /// Shared browser engine instance.
    engine: Arc<BrowserEngine>,
    /// Per-tool session, lazily created and reused across calls.
    session: Arc<Mutex<Option<crate::session::BrowserSession>>>,
    /// Available credentials, loaded at construction time.
    credentials: Arc<Vec<StoredCredential>>,
}

impl BrowserAuthTool {
    /// Create a new browser_auth tool with the given engine and credentials.
    pub fn new(engine: Arc<BrowserEngine>, credentials: Vec<StoredCredential>) -> Self {
        Self {
            engine,
            session: Arc::new(Mutex::new(None)),
            credentials: Arc::new(credentials),
        }
    }

    /// Get or create a browser session.
    async fn get_or_create_session(
        &self,
    ) -> Result<(), BrowserAuthToolError> {
        let mut session_guard = self.session.lock().await;
        if session_guard.is_none() {
            if !self.engine.is_running().await {
                self.engine
                    .start()
                    .await
                    .map_err(|e| BrowserAuthToolError::AuthError(e.to_string()))?;
            }
            *session_guard = Some(self.engine.create_session());
        }
        Ok(())
    }
}

impl Tool for BrowserAuthTool {
    const NAME: &'static str = "browser_auth";

    type Error = BrowserAuthToolError;
    type Args = BrowserAuthToolArgs;
    type Output = BrowserAuthToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let cred_names: Vec<String> = self.credentials.iter().map(|c| c.name.clone()).collect();
        let names_str = if cred_names.is_empty() {
            "No credentials configured. Add them via Settings > Browser Credentials.".to_string()
        } else {
            format!("Available credentials: {}", cred_names.join(", "))
        };

        ToolDefinition {
            name: "browser_auth".to_string(),
            description: format!(
                "Navigate to a URL using stored browser credentials (cookies from a captured \
                 login session). This lets you access authenticated pages without handling \
                 login forms. The credentials were captured by the user via a manual login \
                 in Settings > Browser Credentials. {}",
                names_str
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "credential_name": {
                        "type": "string",
                        "description": format!(
                            "The name of the stored credential to use. {}",
                            names_str
                        )
                    },
                    "url": {
                        "type": "string",
                        "description": "The URL to navigate to. Must start with http:// or https://."
                    }
                },
                "required": ["credential_name", "url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        info!(
            credential = %args.credential_name,
            url = %args.url,
            "browser_auth: authenticating and navigating"
        );

        // Validate URL
        if !args.url.starts_with("http://") && !args.url.starts_with("https://") {
            return Err(BrowserAuthToolError::AuthError(format!(
                "URL must start with http:// or https://, got: {}",
                args.url
            )));
        }

        // Find the credential
        let credential = self
            .credentials
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&args.credential_name))
            .ok_or_else(|| {
                BrowserAuthToolError::AuthError(format!(
                    "No stored credential found with name '{}'. Available: {}",
                    args.credential_name,
                    self.credentials
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;

        // Ensure we have a session
        self.get_or_create_session().await?;

        let mut session_guard = self.session.lock().await;
        let session = session_guard
            .as_mut()
            .expect("session should be initialized");

        // Step 1: Navigate to the target URL first (to establish the domain context)
        let snapshot = session.navigate(&args.url).await.map_err(|e| {
            BrowserAuthToolError::AuthError(format!("Navigation failed: {}", e))
        })?;

        // Step 2: Inject cookies
        let cookie_count = credential.cookies.len();
        debug!(
            cookie_count,
            credential = %credential.name,
            "Injecting cookies"
        );

        session
            .set_cookies(&credential.cookies)
            .await
            .map_err(|e| {
                BrowserAuthToolError::AuthError(format!("Cookie injection failed: {}", e))
            })?;

        // Step 3: Reload the page so the server sees the cookies
        let snapshot = session.navigate(&args.url).await.map_err(|e| {
            BrowserAuthToolError::AuthError(format!("Reload after cookie injection failed: {}", e))
        })?;

        info!(
            title = %snapshot.title,
            url = %snapshot.url,
            cookies_injected = cookie_count,
            "browser_auth: authenticated navigation complete"
        );

        Ok(BrowserAuthToolOutput {
            authenticated: true,
            title: snapshot.title.clone(),
            url: snapshot.url.clone(),
            content: if snapshot.text_content.len() > 2000 {
                format!("{}...", &snapshot.text_content[..2000])
            } else {
                snapshot.text_content.clone()
            },
            cookies_injected: cookie_count,
            page_snapshot: snapshot.to_llm_text(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrowserEngineConfig;

    #[tokio::test]
    async fn test_browser_auth_tool_definition() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(BrowserEngine::new(config));
        let creds = vec![StoredCredential {
            name: "komoot".to_string(),
            url_pattern: "https://komoot.com/*".to_string(),
            cookies: vec![(
                "session".to_string(),
                "abc123".to_string(),
                ".komoot.com".to_string(),
                "/".to_string(),
            )],
        }];

        let tool = BrowserAuthTool::new(engine, creds);
        let def = tool.definition("test".to_string()).await;

        assert_eq!(def.name, "browser_auth");
        assert!(def.description.contains("komoot"));
        assert!(def.description.contains("credentials"));
    }

    #[tokio::test]
    async fn test_browser_auth_tool_mock_call() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(BrowserEngine::new(config));
        let creds = vec![StoredCredential {
            name: "komoot".to_string(),
            url_pattern: "https://komoot.com/*".to_string(),
            cookies: vec![(
                "session".to_string(),
                "abc123".to_string(),
                ".komoot.com".to_string(),
                "/".to_string(),
            )],
        }];

        let tool = BrowserAuthTool::new(engine, creds);
        let result = tool
            .call(BrowserAuthToolArgs {
                credential_name: "komoot".to_string(),
                url: "https://komoot.com/dashboard".to_string(),
            })
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.authenticated);
        assert_eq!(output.cookies_injected, 1);
    }

    #[tokio::test]
    async fn test_browser_auth_tool_missing_credential() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(BrowserEngine::new(config));
        let tool = BrowserAuthTool::new(engine, vec![]);

        let result = tool
            .call(BrowserAuthToolArgs {
                credential_name: "nonexistent".to_string(),
                url: "https://example.com".to_string(),
            })
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No stored credential"));
    }

    #[tokio::test]
    async fn test_browser_auth_tool_invalid_url() {
        let config = BrowserEngineConfig {
            mock_mode: true,
            ..BrowserEngineConfig::default()
        };
        let engine = Arc::new(BrowserEngine::new(config));
        let creds = vec![StoredCredential {
            name: "test".to_string(),
            url_pattern: "https://example.com/*".to_string(),
            cookies: vec![],
        }];
        let tool = BrowserAuthTool::new(engine, creds);

        let result = tool
            .call(BrowserAuthToolArgs {
                credential_name: "test".to_string(),
                url: "ftp://example.com".to_string(),
            })
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("http://"));
    }
}
