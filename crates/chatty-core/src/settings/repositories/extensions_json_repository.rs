use std::path::PathBuf;

use super::extensions_repository::{BoxFuture, ExtensionsRepository};
use super::provider_repository::{RepositoryError, RepositoryResult};
use crate::settings::models::extensions_store::ExtensionsModel;
use tracing::warn;

pub struct ExtensionsJsonRepository {
    file_path: PathBuf,
}

impl ExtensionsJsonRepository {
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;
        let file_path = config_dir.join("chatty").join("extensions.json");
        Ok(Self { file_path })
    }
}

impl ExtensionsRepository for ExtensionsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExtensionsModel>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let exists = tokio::fs::try_exists(&path).await.unwrap_or_else(|e| {
                warn!(error = ?e, path = %path.display(), "Failed to check extensions file");
                false
            });

            if !exists {
                return Ok(ExtensionsModel::default());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let model: ExtensionsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(model)
        })
    }

    fn save(&self, model: ExtensionsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&model)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

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
