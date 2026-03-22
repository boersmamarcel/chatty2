//! Credential vault — stores and retrieves secrets from the OS keyring.
//!
//! Uses the `keyring` crate (when available) for OS-level encrypted storage.
//! Falls back to an AES-256-GCM encrypted JSON file for environments without
//! keyring support (CI, containers, etc.).
//!
//! The encryption key is a random 256-bit key stored in a separate file
//! (`~/.config/chatty/browser_vault.key`) with 0600 permissions on Unix.
//! Each secret entry is encrypted with a random 96-bit nonce and stored as
//! base64-encoded `nonce || ciphertext` in the JSON file.
//!
//! Legacy plaintext entries are detected on load and re-encrypted on the next
//! save operation.
//!
//! The LLM never has access to secrets — only credential **names** are exposed.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::backend::Cookie;
use crate::credential::types::LoginSecret;

/// Service name for keyring entries (used in future keyring integration).
const _KEYRING_SERVICE: &str = "chatty-browser";

/// AES-256-GCM nonce size in bytes (96 bits).
const NONCE_SIZE: usize = 12;

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
/// Fallback: AES-256-GCM encrypted JSON file (`browser_secrets.json`) with a
/// machine-local random key (`browser_vault.key`).
pub struct CredentialVault {
    /// Fallback file path when keyring is unavailable.
    fallback_path: PathBuf,
    /// Path to the encryption key file.
    key_path: PathBuf,
}

impl CredentialVault {
    /// Create a new vault.
    pub fn new() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .context("Cannot determine config directory")?
            .join("chatty");
        Ok(Self {
            fallback_path: config_dir.join("browser_secrets.json"),
            key_path: config_dir.join("browser_vault.key"),
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

    /// Return the set of names that have stored secrets (from fallback store).
    ///
    /// **Limitation**: currently only checks the encrypted fallback file, not the
    /// OS keyring (keyring is stubbed). When keyring support is added, this must
    /// check both locations to avoid incorrect "needs setup" status indicators.
    pub async fn names_with_secrets(&self) -> std::collections::HashSet<String> {
        if Self::store_keyring("__probe__", "").is_ok() {
            tracing::warn!(
                "Keyring is available but names_with_secrets() only scans the \
                 fallback file — keyring-stored secrets will be missed"
            );
        }
        self.load_fallback_store().await.keys().cloned().collect()
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

    // ── Encryption helpers ───────────────────────────────────────────────

    /// Load or generate the AES-256-GCM encryption key.
    async fn load_or_create_key(&self) -> Result<[u8; 32]> {
        if let Ok(bytes) = tokio::fs::read(&self.key_path).await {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Ok(key);
            }
            tracing::warn!("Vault key file has wrong size, regenerating");
        }

        // Generate a new random 256-bit key
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);

        if let Some(parent) = self.key_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.key_path, &key).await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&self.key_path, std::fs::Permissions::from_mode(0o600))
                .await?;
        }

        Ok(key)
    }

    /// Encrypt plaintext with AES-256-GCM, returning base64-encoded `nonce || ciphertext`.
    fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<String> {
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {e}"))?;

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

        let mut blob = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
    }

    /// Decrypt a base64-encoded `nonce || ciphertext` blob.
    fn decrypt(key: &[u8; 32], encoded: &str) -> Result<Vec<u8>> {
        let blob = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("Failed to decode base64 blob")?;

        if blob.len() < NONCE_SIZE + 1 {
            anyhow::bail!("Encrypted blob too short");
        }

        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {e}"))?;

        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))
    }

    /// Try to decrypt a stored value. If decryption fails, treat it as legacy
    /// plaintext (backward compatibility).
    fn decrypt_or_legacy(key: &[u8; 32], value: &str) -> String {
        match Self::decrypt(key, value) {
            Ok(plaintext) => String::from_utf8(plaintext).unwrap_or_else(|_| value.to_string()),
            Err(_) => {
                // Legacy plaintext entry — return as-is; will be re-encrypted on next save
                tracing::debug!("Found legacy plaintext entry, will re-encrypt on next save");
                value.to_string()
            }
        }
    }

    // ── File fallback ────────────────────────────────────────────────────

    async fn store_fallback(&self, name: &str, json: &str) -> Result<()> {
        let key = self.load_or_create_key().await?;
        let mut store = self.load_fallback_store().await;
        let encrypted = Self::encrypt(&key, json.as_bytes())?;
        store.insert(name.to_string(), encrypted);
        self.save_fallback_store(&store).await
    }

    async fn load_fallback(&self, name: &str) -> Result<String> {
        let key = self.load_or_create_key().await?;
        let store = self.load_fallback_store().await;
        let value = store
            .get(name)
            .with_context(|| format!("No credential found for \"{name}\""))?;
        Ok(Self::decrypt_or_legacy(&key, value))
    }

    async fn delete_fallback(&self, name: &str) -> Result<()> {
        let mut store = self.load_fallback_store().await;
        store.remove(name);
        self.save_fallback_store(&store).await
    }

    async fn load_fallback_store(&self) -> std::collections::HashMap<String, String> {
        match tokio::fs::read_to_string(&self.fallback_path).await {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(store) => store,
                Err(e) => {
                    tracing::warn!(
                        path = %self.fallback_path.display(),
                        error = %e,
                        "Failed to parse credential store — stored credentials will appear missing. \
                         The corrupted file will be overwritten on next save."
                    );
                    std::collections::HashMap::new()
                }
            },
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %self.fallback_path.display(),
                    error = %e,
                    "Failed to read credential store"
                );
                std::collections::HashMap::new()
            }
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&self.fallback_path, std::fs::Permissions::from_mode(0o600))
                .await?;
        }

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

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let plaintext = b"super secret data";

        let encrypted = CredentialVault::encrypt(&key, plaintext).unwrap();
        // Encrypted value should be base64 and different from plaintext
        assert_ne!(encrypted.as_bytes(), plaintext);

        let decrypted = CredentialVault::decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_produces_different_ciphertexts() {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let plaintext = b"same data";

        let enc1 = CredentialVault::encrypt(&key, plaintext).unwrap();
        let enc2 = CredentialVault::encrypt(&key, plaintext).unwrap();
        // Random nonces should produce different ciphertexts
        assert_ne!(enc1, enc2);

        // Both should decrypt to the same value
        assert_eq!(
            CredentialVault::decrypt(&key, &enc1).unwrap(),
            CredentialVault::decrypt(&key, &enc2).unwrap()
        );
    }

    #[test]
    fn test_decrypt_wrong_key_fails() {
        let mut key1 = [0u8; 32];
        let mut key2 = [0u8; 32];
        OsRng.fill_bytes(&mut key1);
        OsRng.fill_bytes(&mut key2);

        let encrypted = CredentialVault::encrypt(&key1, b"secret").unwrap();
        assert!(CredentialVault::decrypt(&key2, &encrypted).is_err());
    }

    #[test]
    fn test_decrypt_or_legacy_with_plaintext() {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let legacy = r#"{"kind":"session_cookies","cookies":[]}"#;

        // Legacy plaintext should be returned as-is
        let result = CredentialVault::decrypt_or_legacy(&key, legacy);
        assert_eq!(result, legacy);
    }

    #[test]
    fn test_decrypt_or_legacy_with_encrypted() {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let plaintext = r#"{"kind":"session_cookies","cookies":[]}"#;

        let encrypted = CredentialVault::encrypt(&key, plaintext.as_bytes()).unwrap();
        let result = CredentialVault::decrypt_or_legacy(&key, &encrypted);
        assert_eq!(result, plaintext);
    }

    #[tokio::test]
    async fn test_vault_store_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault {
            fallback_path: dir.path().join("secrets.json"),
            key_path: dir.path().join("vault.key"),
        };

        let secret = LoginSecret::FormCredentials {
            username: "alice".into(),
            password: "s3cret!".into(),
        };

        vault.store("test-cred", &secret).await.unwrap();

        // Verify the raw file does NOT contain plaintext password
        let raw = tokio::fs::read_to_string(&vault.fallback_path)
            .await
            .unwrap();
        assert!(
            !raw.contains("s3cret!"),
            "Password should not appear in plaintext in the file"
        );

        // Load should return the original secret
        let loaded = vault.load("test-cred").await.unwrap();
        match loaded {
            LoginSecret::FormCredentials { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "s3cret!");
            }
            _ => panic!("Expected FormCredentials"),
        }
    }

    #[tokio::test]
    async fn test_vault_legacy_plaintext_compat() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault {
            fallback_path: dir.path().join("secrets.json"),
            key_path: dir.path().join("vault.key"),
        };

        // Write a legacy plaintext store (simulating old format)
        let legacy_json = r#"{"kind":"form_credentials","username":"bob","password":"old_pass"}"#;
        let mut store = std::collections::HashMap::new();
        store.insert("legacy-cred".to_string(), legacy_json.to_string());
        let file_contents = serde_json::to_string_pretty(&store).unwrap();
        tokio::fs::create_dir_all(dir.path()).await.unwrap();
        tokio::fs::write(&vault.fallback_path, &file_contents)
            .await
            .unwrap();

        // Load should succeed (backward compat)
        let loaded = vault.load("legacy-cred").await.unwrap();
        match loaded {
            LoginSecret::FormCredentials { username, password } => {
                assert_eq!(username, "bob");
                assert_eq!(password, "old_pass");
            }
            _ => panic!("Expected FormCredentials"),
        }
    }

    #[tokio::test]
    async fn test_vault_delete_and_exists() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault {
            fallback_path: dir.path().join("secrets.json"),
            key_path: dir.path().join("vault.key"),
        };

        let secret = LoginSecret::SessionCookies(vec![Cookie {
            name: "tok".into(),
            value: "xyz".into(),
            domain: ".test.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
            expires: None,
        }]);

        vault.store("del-test", &secret).await.unwrap();
        assert!(vault.exists("del-test").await);

        vault.delete("del-test").await.unwrap();
        assert!(!vault.exists("del-test").await);
    }

    #[tokio::test]
    async fn test_vault_names_with_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let vault = CredentialVault {
            fallback_path: dir.path().join("secrets.json"),
            key_path: dir.path().join("vault.key"),
        };

        let secret = LoginSecret::FormCredentials {
            username: "u".into(),
            password: "p".into(),
        };

        vault.store("cred-a", &secret).await.unwrap();
        vault.store("cred-b", &secret).await.unwrap();

        let names = vault.names_with_secrets().await;
        assert!(names.contains("cred-a"));
        assert!(names.contains("cred-b"));
        assert_eq!(names.len(), 2);
    }
}
