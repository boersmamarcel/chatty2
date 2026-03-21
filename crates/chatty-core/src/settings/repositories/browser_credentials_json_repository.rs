use std::path::PathBuf;

use super::browser_credentials_repository::{BoxFuture, BrowserCredentialsRepository};
use super::provider_repository::{RepositoryError, RepositoryResult};
use crate::settings::models::browser_credentials_store::BrowserCredentialsModel;

pub struct BrowserCredentialsJsonRepository {
    file_path: PathBuf,
}

impl BrowserCredentialsJsonRepository {
    /// Create repository with XDG-compliant path.
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("browser_credentials.json");

        Ok(Self { file_path })
    }

    /// Create repository with a custom file path (for testing).
    #[cfg(test)]
    pub(crate) fn with_path(file_path: PathBuf) -> Self {
        Self { file_path }
    }
}

impl BrowserCredentialsRepository for BrowserCredentialsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<BrowserCredentialsModel>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(BrowserCredentialsModel::default());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let model: BrowserCredentialsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(model)
        })
    }

    fn save(
        &self,
        credentials: BrowserCredentialsModel,
    ) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&credentials)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            // Write atomically using temp file + rename
            let temp_path = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
            tokio::fs::write(&temp_path, &json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::browser_credentials_store::{
        AuthType, CapturedCookie, WebCredential,
    };

    #[tokio::test]
    async fn test_repository_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "chatty_browser_creds_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("browser_credentials.json");

        let repo = BrowserCredentialsJsonRepository::with_path(path);

        let model = BrowserCredentialsModel {
            credentials: vec![WebCredential {
                name: "komoot".to_string(),
                url_pattern: "https://komoot.com/*".to_string(),
                auth_type: AuthType::CapturedSession {
                    cookies: vec![CapturedCookie {
                        name: "session_id".to_string(),
                        value: "abc123".to_string(),
                        domain: ".komoot.com".to_string(),
                        path: "/".to_string(),
                    }],
                    captured_at: "2026-03-21T08:00:00Z".to_string(),
                },
            }],
        };

        repo.save(model.clone()).await.unwrap();
        let loaded = repo.load().await.unwrap();

        assert_eq!(loaded.credentials.len(), 1);
        assert_eq!(loaded.credentials[0].name, "komoot");
        assert_eq!(
            loaded.credentials[0].url_pattern,
            "https://komoot.com/*"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn test_repository_load_missing_file() {
        let dir = std::env::temp_dir().join(format!(
            "chatty_browser_creds_test_missing_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nonexistent.json");

        let repo = BrowserCredentialsJsonRepository::with_path(path);
        let loaded = repo.load().await.unwrap();

        assert!(loaded.credentials.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
