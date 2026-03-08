use std::path::PathBuf;

use super::provider_repository::{RepositoryError, RepositoryResult};
use super::user_secrets_repository::{BoxFuture, UserSecretsRepository};
use crate::settings::models::user_secrets_store::UserSecretsModel;

pub struct UserSecretsJsonRepository {
    file_path: PathBuf,
}

impl UserSecretsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("user_secrets.json");

        Ok(Self { file_path })
    }

    /// Create repository with a custom file path (for testing)
    #[cfg(test)]
    pub(crate) fn with_path(file_path: PathBuf) -> Self {
        Self { file_path }
    }
}

impl UserSecretsRepository for UserSecretsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<UserSecretsModel>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(UserSecretsModel::default());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let model: UserSecretsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(model)
        })
    }

    fn save(&self, secrets: UserSecretsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&secrets)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            // Write atomically using temp file + rename
            let temp_path = path.with_extension(format!("json.{}.tmp", std::process::id()));
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
    use crate::settings::models::user_secrets_store::{UserSecret, UserSecretsModel};
    use crate::settings::repositories::user_secrets_repository::UserSecretsRepository;

    #[tokio::test]
    async fn test_repository_save_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("chatty_secrets_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user_secrets.json");

        let repo = UserSecretsJsonRepository::with_path(path);

        let model = UserSecretsModel {
            secrets: vec![
                UserSecret {
                    key: "KEY_A".into(),
                    value: "value_a".into(),
                },
                UserSecret {
                    key: "KEY_B".into(),
                    value: "val with 'quotes'".into(),
                },
            ],
            ..Default::default()
        };

        repo.save(model.clone()).await.unwrap();
        let loaded = repo.load().await.unwrap();

        assert_eq!(loaded.secrets.len(), 2);
        assert_eq!(loaded.secrets[0].key, "KEY_A");
        assert_eq!(loaded.secrets[0].value, "value_a");
        assert_eq!(loaded.secrets[1].key, "KEY_B");
        assert_eq!(loaded.secrets[1].value, "val with 'quotes'");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
