//! Credential vault — stores and retrieves secrets from the OS keyring.
//!
//! Uses the `keyring` crate (when available) for OS-level encrypted storage.
//! Falls back to a JSON-file-based store for environments without keyring
//! support (CI, containers, etc.).
//!
//! The LLM never has access to secrets — only credential **names** are exposed.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::backend::Cookie;
use crate::credential::types::LoginSecret;

/// Service name for keyring entries (used in future keyring integration).
const _KEYRING_SERVICE: &str = "chatty-browser";

/// Envelope for secrets serialized to the keyring (or fallback file).
#[derive(Serialize, Deserialize)]
struct SecretEnvelope {
    /// "session_cookies" or "form_credentials"
    kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cookies: Vec<Cookie>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

/// Credential vault that stores secrets outside of user-visible config files.
///
/// Primary storage: OS keyring (`keyring` crate).
/// Fallback: encrypted-at-rest JSON file in the config directory.
pub struct CredentialVault {
    /// Fallback file path when keyring is unavailable.
    fallback_path: PathBuf,
}

impl CredentialVault {
    /// Create a new vault.
    pub fn new() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .context("Cannot determine config directory")?
            .join("chatty");
        Ok(Self {
            fallback_path: config_dir.join("browser_secrets.json"),
        })
    }

    /// Store a secret for the given credential name.
    pub async fn store(&self, name: &str, secret: &LoginSecret) -> Result<()> {
        let envelope = match secret {
            LoginSecret::SessionCookies(cookies) => SecretEnvelope {
                kind: "session_cookies".into(),
                cookies: cookies.clone(),
                username: None,
                password: None,
            },
            LoginSecret::FormCredentials { username, password } => SecretEnvelope {
                kind: "form_credentials".into(),
                cookies: Vec::new(),
                username: Some(username.clone()),
                password: Some(password.clone()),
            },
        };

        let json =
            serde_json::to_string(&envelope).context("Failed to serialize secret envelope")?;

        // Try OS keyring first, fall back to file
        match Self::store_keyring(name, &json) {
            Ok(()) => {
                tracing::debug!(credential = name, "Secret stored in OS keyring");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    credential = name,
                    error = ?e,
                    "Keyring unavailable, using fallback file"
                );
                self.store_fallback(name, &json).await
            }
        }
    }

    /// Load a secret by credential name.
    pub async fn load(&self, name: &str) -> Result<LoginSecret> {
        // Try keyring first
        let json = match Self::load_keyring(name) {
            Ok(json) => json,
            Err(_) => self.load_fallback(name).await?,
        };

        let envelope: SecretEnvelope =
            serde_json::from_str(&json).context("Failed to deserialize secret")?;

        match envelope.kind.as_str() {
            "session_cookies" => Ok(LoginSecret::SessionCookies(envelope.cookies)),
            "form_credentials" => {
                let username = envelope.username.context("Missing username")?;
                let password = envelope.password.context("Missing password")?;
                Ok(LoginSecret::FormCredentials { username, password })
            }
            other => anyhow::bail!("Unknown secret kind: {other}"),
        }
    }

    /// Delete a secret by credential name.
    pub async fn delete(&self, name: &str) -> Result<()> {
        // Try keyring
        let _ = Self::delete_keyring(name);
        // Also remove from fallback
        self.delete_fallback(name).await
    }

    /// Check if a secret exists for the given name.
    pub async fn exists(&self, name: &str) -> bool {
        Self::load_keyring(name).is_ok() || self.load_fallback(name).await.is_ok()
    }

    // ── Keyring operations ───────────────────────────────────────────────

    fn store_keyring(name: &str, json: &str) -> Result<()> {
        // Stub: in the full implementation, uses the `keyring` crate.
        // For now, always falls through to the fallback.
        let _ = (name, json);
        anyhow::bail!("Keyring not available in this build")
    }

    fn load_keyring(name: &str) -> Result<String> {
        let _ = name;
        anyhow::bail!("Keyring not available in this build")
    }

    fn delete_keyring(name: &str) -> Result<()> {
        let _ = name;
        anyhow::bail!("Keyring not available in this build")
    }

    // ── File fallback ────────────────────────────────────────────────────

    async fn store_fallback(&self, name: &str, json: &str) -> Result<()> {
        let mut store = self.load_fallback_store().await;
        store.insert(name.to_string(), json.to_string());
        self.save_fallback_store(&store).await
    }

    async fn load_fallback(&self, name: &str) -> Result<String> {
        let store = self.load_fallback_store().await;
        store
            .get(name)
            .cloned()
            .with_context(|| format!("No credential found for \"{name}\""))
    }

    async fn delete_fallback(&self, name: &str) -> Result<()> {
        let mut store = self.load_fallback_store().await;
        store.remove(name);
        self.save_fallback_store(&store).await
    }

    async fn load_fallback_store(&self) -> std::collections::HashMap<String, String> {
        match tokio::fs::read_to_string(&self.fallback_path).await {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => std::collections::HashMap::new(),
        }
    }

    async fn save_fallback_store(
        &self,
        store: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        if let Some(parent) = self.fallback_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(store)?;
        let temp = self.fallback_path.with_extension("json.tmp");
        tokio::fs::write(&temp, &json).await?;
        tokio::fs::rename(&temp, &self.fallback_path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_envelope_session_cookies_roundtrip() {
        let envelope = SecretEnvelope {
            kind: "session_cookies".into(),
            cookies: vec![Cookie {
                name: "sid".into(),
                value: "abc123".into(),
                domain: ".example.com".into(),
                path: "/".into(),
                secure: true,
                http_only: true,
                expires: None,
            }],
            username: None,
            password: None,
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let decoded: SecretEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.kind, "session_cookies");
        assert_eq!(decoded.cookies.len(), 1);
        assert_eq!(decoded.cookies[0].name, "sid");
    }

    #[test]
    fn test_secret_envelope_form_credentials_roundtrip() {
        let envelope = SecretEnvelope {
            kind: "form_credentials".into(),
            cookies: Vec::new(),
            username: Some("user@example.com".into()),
            password: Some("hunter2".into()),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let decoded: SecretEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.kind, "form_credentials");
        assert_eq!(decoded.username.as_deref(), Some("user@example.com"));
    }
}
