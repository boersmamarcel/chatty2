use std::path::PathBuf;

use super::oauth_credential_repository::OAuthCredentialRepository;
use super::provider_repository::{BoxFuture, RepositoryError, RepositoryResult};

/// JSON-file-backed implementation of [`OAuthCredentialRepository`].
///
/// Each server's credentials are stored in a separate file:
/// `<config_dir>/chatty/mcp_oauth_<sanitized_name>.json`
pub struct JsonOAuthCredentialRepository {
    dir: PathBuf,
}

impl JsonOAuthCredentialRepository {
    /// Create repository with XDG-compliant path.
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let dir = config_dir.join("chatty");

        Ok(Self { dir })
    }

    /// Create repository with a custom directory (for testing).
    pub fn new_with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn credential_path(dir: &std::path::Path, server_name: &str) -> PathBuf {
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
        dir.join(format!("mcp_oauth_{sanitized}.json"))
    }
}

impl OAuthCredentialRepository for JsonOAuthCredentialRepository {
    fn load(
        &self,
        server_name: &str,
    ) -> BoxFuture<'static, RepositoryResult<Option<serde_json::Value>>> {
        let path = Self::credential_path(&self.dir, server_name);

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(None);
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            match serde_json::from_str(&contents) {
                Ok(value) => Ok(Some(value)),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "Corrupt OAuth credentials file, removing"
                    );
                    tokio::fs::remove_file(&path).await.ok();
                    Ok(None)
                }
            }
        })
    }

    fn save(
        &self,
        server_name: &str,
        credentials: serde_json::Value,
    ) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = Self::credential_path(&self.dir, server_name);

        Box::pin(async move {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            let json = serde_json::to_string_pretty(&credentials)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            // Atomic write: temp file + rename
            let temp_path = path.with_extension("json.tmp");
            tokio::fs::write(&temp_path, &json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tracing::debug!(path = %path.display(), "Saved OAuth credentials");
            Ok(())
        })
    }

    fn clear(&self, server_name: &str) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = Self::credential_path(&self.dir, server_name);

        Box::pin(async move {
            if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                tokio::fs::remove_file(&path)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
                tracing::debug!(path = %path.display(), "Cleared OAuth credentials");
            }
            Ok(())
        })
    }

    fn has_credentials(&self, server_name: &str) -> bool {
        Self::credential_path(&self.dir, server_name).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_repo(dir: &TempDir) -> JsonOAuthCredentialRepository {
        JsonOAuthCredentialRepository {
            dir: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn test_load_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let repo = test_repo(&dir);
        assert!(repo.load("test").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let repo = test_repo(&dir);

        let creds = serde_json::json!({
            "client_id": "test-client",
            "token_response": null,
            "granted_scopes": ["read"]
        });

        repo.save("test", creds.clone()).await.unwrap();
        let loaded = repo.load("test").await.unwrap().unwrap();
        assert_eq!(loaded["client_id"], "test-client");
        assert_eq!(loaded["granted_scopes"], serde_json::json!(["read"]));
    }

    #[tokio::test]
    async fn test_clear_removes_file() {
        let dir = TempDir::new().unwrap();
        let repo = test_repo(&dir);

        let creds = serde_json::json!({"client_id": "test"});
        repo.save("test", creds).await.unwrap();
        assert!(repo.has_credentials("test"));

        repo.clear("test").await.unwrap();
        assert!(!repo.has_credentials("test"));
    }

    #[tokio::test]
    async fn test_clear_nonexistent_is_ok() {
        let dir = TempDir::new().unwrap();
        let repo = test_repo(&dir);
        assert!(repo.clear("test").await.is_ok());
    }

    #[tokio::test]
    async fn test_corrupt_file_returns_none_and_cleans_up() {
        let dir = TempDir::new().unwrap();
        let repo = test_repo(&dir);

        let path =
            JsonOAuthCredentialRepository::credential_path(&dir.path().to_path_buf(), "test");
        tokio::fs::write(&path, "not valid json").await.unwrap();
        assert!(repo.has_credentials("test"));

        let result = repo.load("test").await.unwrap();
        assert!(result.is_none());
        assert!(!repo.has_credentials("test")); // cleaned up
    }

    #[test]
    fn test_sanitizes_server_name() {
        let dir = TempDir::new().unwrap();
        let path = JsonOAuthCredentialRepository::credential_path(
            &dir.path().to_path_buf(),
            "my server/with:special.chars",
        );
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "mcp_oauth_my_server_with_special_chars.json");
    }

    #[tokio::test]
    async fn test_multiple_servers_isolated() {
        let dir = TempDir::new().unwrap();
        let repo = test_repo(&dir);

        repo.save("server-a", serde_json::json!({"client_id": "a"}))
            .await
            .unwrap();
        repo.save("server-b", serde_json::json!({"client_id": "b"}))
            .await
            .unwrap();

        let a = repo.load("server-a").await.unwrap().unwrap();
        let b = repo.load("server-b").await.unwrap().unwrap();
        assert_eq!(a["client_id"], "a");
        assert_eq!(b["client_id"], "b");

        repo.clear("server-a").await.unwrap();
        assert!(!repo.has_credentials("server-a"));
        assert!(repo.has_credentials("server-b"));
    }
}
