use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use std::path::PathBuf;
use tracing::{debug, warn};

/// File-based credential store for MCP OAuth tokens.
///
/// Tokens are persisted as JSON files in the app's config directory alongside
/// `mcp_servers.json`. Each server gets its own file: `mcp_oauth_{name}.json`.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    /// Create a store for the given MCP server name.
    pub fn for_server(server_name: &str) -> Self {
        let sanitized = server_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();

        let dir = Self::credentials_dir();
        Self {
            path: dir.join(format!("mcp_oauth_{sanitized}.json")),
        }
    }

    /// Delete stored credentials for a server.
    pub async fn delete_for_server(server_name: &str) {
        let store = Self::for_server(server_name);
        if store.path.exists() {
            if let Err(e) = tokio::fs::remove_file(&store.path).await {
                warn!(
                    server = %server_name,
                    error = ?e,
                    "Failed to delete OAuth credentials file"
                );
            } else {
                debug!(server = %server_name, "Deleted OAuth credentials");
            }
        }
    }

    /// Check if stored credentials exist for a server (without loading them).
    pub fn has_credentials(server_name: &str) -> bool {
        Self::for_server(server_name).path.exists()
    }

    fn credentials_dir() -> PathBuf {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("chatty");
        // Ensure dir exists (best-effort)
        std::fs::create_dir_all(&dir).ok();
        dir
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        if !self.path.exists() {
            return Ok(None);
        }

        match tokio::fs::read_to_string(&self.path).await {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(creds) => {
                    debug!(path = %self.path.display(), "Loaded OAuth credentials from file");
                    Ok(Some(creds))
                }
                Err(e) => {
                    warn!(
                        path = %self.path.display(),
                        error = ?e,
                        "Corrupt OAuth credentials file, removing"
                    );
                    tokio::fs::remove_file(&self.path).await.ok();
                    Ok(None)
                }
            },
            Err(e) => {
                warn!(path = %self.path.display(), error = ?e, "Failed to read OAuth credentials");
                Ok(None)
            }
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string_pretty(&credentials).map_err(|e| {
            AuthError::InternalError(format!("Failed to serialize credentials: {e}"))
        })?;

        tokio::fs::write(&self.path, json).await.map_err(|e| {
            AuthError::InternalError(format!(
                "Failed to write credentials to {}: {e}",
                self.path.display()
            ))
        })?;

        debug!(path = %self.path.display(), "Saved OAuth credentials to file");
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        if self.path.exists() {
            tokio::fs::remove_file(&self.path).await.map_err(|e| {
                AuthError::InternalError(format!(
                    "Failed to remove credentials file {}: {e}",
                    self.path.display()
                ))
            })?;
            debug!(path = %self.path.display(), "Cleared OAuth credentials");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir, name: &str) -> FileCredentialStore {
        FileCredentialStore {
            path: dir.path().join(format!("mcp_oauth_{name}.json")),
        }
    }

    #[tokio::test]
    async fn test_load_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir, "test");
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir, "test");

        let creds = StoredCredentials::new(
            "test-client".to_string(),
            None,
            vec!["read".to_string()],
            None,
        );

        store.save(creds.clone()).await.unwrap();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.client_id, "test-client");
        assert_eq!(loaded.granted_scopes, vec!["read"]);
    }

    #[tokio::test]
    async fn test_clear_removes_file() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir, "test");

        let creds = StoredCredentials::new("test-client".to_string(), None, vec![], None);

        store.save(creds).await.unwrap();
        assert!(store.path.exists());

        store.clear().await.unwrap();
        assert!(!store.path.exists());
    }

    #[tokio::test]
    async fn test_clear_nonexistent_is_ok() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir, "test");
        assert!(store.clear().await.is_ok());
    }

    #[tokio::test]
    async fn test_corrupt_file_returns_none_and_cleans_up() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir, "test");

        tokio::fs::write(&store.path, "not valid json")
            .await
            .unwrap();
        assert!(store.path.exists());

        let result = store.load().await.unwrap();
        assert!(result.is_none());
        assert!(!store.path.exists()); // cleaned up
    }

    #[test]
    fn test_sanitizes_server_name() {
        let store = FileCredentialStore::for_server("my server/with:special.chars");
        let filename = store.path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "mcp_oauth_my_server_with_special_chars.json");
    }
}
